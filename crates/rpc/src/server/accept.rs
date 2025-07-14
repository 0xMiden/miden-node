use std::task::{Context as StdContext, Poll};

use futures::{FutureExt, future::BoxFuture};
use http::{
    HeaderMap, HeaderValue,
    header::{ACCEPT, ToStrError},
};
use miden_node_utils::ErrorReport;
use miden_objects::Digest;
use miden_tx::utils::HexParseError;
use nom::bytes::complete::{tag, take_until};
use semver::{Version, VersionReq};
use tower::{Layer, Service};

/// Rejects requests whose headers indicate a mismatch in RPC version or the network.
///
/// The former is taken from the `accept` header with the expected format
/// `application/vnd.miden.<major>.<minor>.<patch>+grpc`.
///
/// The latter expects the network's genesis block's commitment in the `miden-network` header.
///
/// Missing values are ignored.
#[derive(Clone)]
pub struct HeaderVerificationLayer {
    version_req: VersionReq,
    genesis_commitment: Digest,
}

#[derive(Debug, thiserror::Error)]
enum VerificationError {
    #[error("HTTP header verification failed")]
    Accept(#[source] AcceptHeaderError),

    #[error("Network verification failed")]
    Network(#[source] NetworkError),
}

#[derive(Debug, thiserror::Error)]
enum AcceptHeaderError {
    #[error("Header value could not be parsed as a string")]
    InvalidValue(#[source] ToStrError),

    #[error("ACCEPT header could not be parsed")]
    InvalidFormat,

    #[error("RPC version could not be parsed as semver")]
    InvalidSemver(#[source] semver::Error),

    #[error("RPC version {user} is not supported, only {server} is supported")]
    UnsupportedVersion { user: Version, server: VersionReq },
}

#[derive(Debug, thiserror::Error)]
enum NetworkError {
    #[error("Header value could not be parsed as a string")]
    InvalidValue(#[source] ToStrError),

    #[error("Failed to parse genesis commitment")]
    InvalidFormat(#[source] HexParseError),

    #[error("The provided genesis block commitment {user} does not match the servers {server}")]
    IncorrectCommitment { user: Digest, server: Digest },
}

impl HeaderVerificationLayer {
    pub fn new(rpc_version: &Version, genesis_commitment: Digest) -> Self {
        // Form a version requirement from the major and minor version numbers.
        let version_req = format!("={}.{}", rpc_version.major, rpc_version.minor);
        let version_req = VersionReq::parse(&version_req).expect("valid version requirement");
        HeaderVerificationLayer { version_req, genesis_commitment }
    }
}

impl<S> Layer<S> for HeaderVerificationLayer {
    type Service = HeaderVerificationService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        HeaderVerificationService { inner, verifier: self.clone() }
    }
}

impl HeaderVerificationLayer {
    const NETWORK_HEADER: &str = "miden-network";

    fn verify(&self, headers: &HeaderMap<HeaderValue>) -> Result<(), VerificationError> {
        self.verify_accept_header(headers).map_err(VerificationError::Accept)?;
        self.verify_genesis_commitment(headers).map_err(VerificationError::Network)
    }

    fn verify_accept_header(
        &self,
        headers: &HeaderMap<HeaderValue>,
    ) -> Result<(), AcceptHeaderError> {
        let Some(header) = headers.get(ACCEPT) else {
            return Ok(());
        };

        // Grab the header's value
        let value = header.to_str().map_err(AcceptHeaderError::InvalidValue)?;
        let value =
            AcceptHeaderValue::try_from(value).map_err(|_| AcceptHeaderError::InvalidFormat)?;
        let version = Version::parse(value.version).map_err(AcceptHeaderError::InvalidSemver)?;

        if !self.version_req.matches(&version) {
            return Err(AcceptHeaderError::UnsupportedVersion {
                user: version,
                server: self.version_req.clone(),
            });
        }

        Ok(())
    }

    fn verify_genesis_commitment(
        &self,
        headers: &HeaderMap<HeaderValue>,
    ) -> Result<(), NetworkError> {
        let Some(header) = headers.get(Self::NETWORK_HEADER) else {
            return Ok(());
        };

        let value = header.to_str().map_err(NetworkError::InvalidValue)?;
        let commitment = Digest::try_from(value).map_err(NetworkError::InvalidFormat)?;

        if commitment != self.genesis_commitment {
            return Err(NetworkError::IncorrectCommitment {
                user: commitment,
                server: self.genesis_commitment,
            });
        }

        Ok(())
    }
}

/// Service responsible for handling HTTP ACCEPT headers.
#[derive(Clone)]
pub struct HeaderVerificationService<S> {
    inner: S,
    verifier: HeaderVerificationLayer,
}

impl<S, B> Service<http::Request<B>> for HeaderVerificationService<S>
where
    S: Service<http::Request<B>, Response = http::Response<B>> + Clone + Send + 'static,
    S::Error: Send + 'static,
    S::Future: Send + 'static,
    B: Default + Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut StdContext<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    /// Validates existence and content of HTTP ACCEPT header.
    ///
    /// The version specified in the value of the ACCEPT header must match the version requirements
    /// specified by the server.
    fn call(&mut self, request: http::Request<B>) -> Self::Future {
        if let Err(err) = self.verifier.verify(request.headers()) {
            let response = tonic::Status::invalid_argument(err.as_report()).into_http();

            return futures::future::ready(Ok(response)).boxed();
        }

        self.inner.call(request).boxed()
    }
}

/// The result of parsing the following canonical representation of an ACCEPT header value:
/// `application/vnd.{app_name}.{version}+{response_type}`.
#[derive(Debug, PartialEq)]
pub struct AcceptHeaderValue<'a> {
    app_name: &'a str,
    version: &'a str,
    response_type: &'a str,
}

impl<'a> TryFrom<&'a str> for AcceptHeaderValue<'a> {
    type Error = nom::Err<nom::error::Error<&'a str>>;

    fn try_from(input: &'a str) -> Result<Self, Self::Error> {
        let (input, _) = tag("application/vnd.")(input)?;
        let (input, app_name) = take_until(".")(input)?;
        let (input, _) = tag(".")(input)?;
        let (input, version) = take_until("+")(input)?;
        let (response_type, _) = tag("+")(input)?;

        Ok(AcceptHeaderValue { app_name, version, response_type })
    }
}

// HEADER VERIFICATION TESTS
// ================================================================================================

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderName, HeaderValue};
    use miden_objects::Digest;
    use semver::Version;

    use super::HeaderVerificationLayer;

    const TEST_GENESIS_COMMITMENT: &str =
        "0x00000000000000000000000000000000000000000000000000000000deadbeef";
    const TEST_RPC_VERSION: Version = Version::new(1, 2, 3);

    impl HeaderVerificationLayer {
        fn for_tests() -> Self {
            Self::new(&TEST_RPC_VERSION, Digest::try_from(TEST_GENESIS_COMMITMENT).unwrap())
        }
    }

    #[rstest::rstest]
    #[case::without_headers(&[])]
    #[case::with_matching_version(&[
        ("accept", "application/vnd.miden.1.2.3+grpc")
    ])]
    #[case::with_newer_patch(&[
        ("accept", "application/vnd.miden.1.2.4+grpc")
    ])]
    #[case::with_older_patch_version(&[
        ("accept", "application/vnd.miden.1.2.2+grpc")
    ])]
    #[case::with_matching_network(&[
        ("miden-network", TEST_GENESIS_COMMITMENT)
    ])]
    #[case::with_matching_network_and_version(&[
        ("miden-network", TEST_GENESIS_COMMITMENT),
        ("accept", "application/vnd.miden.1.2.3+grpc")
    ])]
    #[test]
    fn request_should_pass(#[case] headers: &[(&'static str, &'static str)]) {
        let headers = headers
            .iter()
            .map(|(k, v)| (HeaderName::from_static(k), HeaderValue::from_static(v)));
        let headers = HeaderMap::from_iter(headers);

        let uut = HeaderVerificationLayer::for_tests();
        uut.verify(&headers).unwrap();
    }

    #[rstest::rstest]
    #[case::with_older_minor_version(&[
        ("accept", "application/vnd.miden.1.1.100+grpc")
    ])]
    #[case::with_newer_minor_version(&[
        ("accept", "application/vnd.miden.1.3.0+grpc")
    ])]
    #[case::with_older_minor_version_and_matching_network(&[
        ("accept", "application/vnd.miden.1.1.100+grpc"),
        ("miden-network", TEST_GENESIS_COMMITMENT),
    ])]
    #[case::with_network_mismatch(&[
        ("miden-network", "0x00000000000000000000000000000000000000000000000000000000dead1234"),
    ])]
    #[case::with_network_mismatch_but_matching_version(&[
        ("accept", "application/vnd.miden.1.2.3+grpc"),
        ("miden-network", "0x00000000000000000000000000000000000000000000000000000000dead1234"),
    ])]
    #[case::invalid_version(&[
        ("accept", "application/vnd.miden.x.y.z+grpc"),
    ])]
    #[case::without_grpc_version_suffix(&[
        ("accept", "application/vnd.miden.1.2.3"),
    ])]
    #[test]
    fn request_should_be_rejected(#[case] headers: &[(&'static str, &'static str)]) {
        let headers = headers
            .iter()
            .map(|(k, v)| (HeaderName::from_static(k), HeaderValue::from_static(v)));
        let headers = HeaderMap::from_iter(headers);

        let uut = HeaderVerificationLayer::for_tests();
        uut.verify(&headers).unwrap_err();
    }
}
