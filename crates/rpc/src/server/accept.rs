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
use tracing::debug;

use crate::COMPONENT;

/// Layer responsible for handling HTTP ACCEPT headers.
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
    /// Create a new accept layer that validates version values specified in the HTTP ACCEPT header.
    ///
    /// The version requirement is based on the version field found in the workspace's Cargo.toml.
    ///
    /// # Panics:
    ///
    /// Panics if the version string in Cargo.toml is not valid semver. The version string is made
    /// into an env var at compile time which means that the unit tests prove this cannot panic
    /// in practice.
    pub fn new(genesis_commitment: Digest) -> anyhow::Result<Self> {
        // Parse the full version string (e.g. "0.8.0").
        let version = env!("CARGO_PKG_VERSION");
        let version = Version::parse(version)?;

        // Form a version requirement from the major and minor version numbers.
        let version_req = format!("={}.{}", version.major, version.minor);
        let version_req = VersionReq::parse(&version_req).expect("valid version requirement");
        Ok(HeaderVerificationLayer { version_req, genesis_commitment })
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
            let response = http::Response::builder()
                    .status(http::StatusCode::BAD_REQUEST)
                    .header("content-type", "application/grpc")
                    .header("grpc-status", "3") // INVALID_ARGUMENT
                    .header("grpc-message", err.as_report())
                    .body(B::default())
                    .expect("response should build as all parts are valid");

            return futures::future::ready(Ok(response)).boxed();
        };

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
    use semver::Version;

    use super::{AcceptHeaderValue, HeaderVerificationLayer};

    #[test]
    fn current_version_is_parsed_and_matches() {
        let a = HeaderVerificationLayer::new().unwrap();
        let version = env!("CARGO_PKG_VERSION");
        let version = Version::parse(version).unwrap();
        assert!(a.version_req.matches(&version));
    }

    #[test]
    fn same_minor_different_patch_matches() {
        let a = HeaderVerificationLayer::new().unwrap();
        let version = env!("CARGO_PKG_VERSION");
        let mut version = Version::parse(version).unwrap();
        version.patch += 1;
        assert!(a.version_req.matches(&version));
    }

    #[test]
    fn greater_minor_does_not_match() {
        let a = HeaderVerificationLayer::new().unwrap();
        let version = Version::parse("0.99.0").unwrap();
        assert!(!a.version_req.matches(&version));
    }

    #[test]
    fn lower_minor_does_not_match() {
        let a = HeaderVerificationLayer::new().unwrap();
        let version = Version::parse("0.1.0").unwrap();
        assert!(!a.version_req.matches(&version));
    }

    #[test]
    fn greater_major_does_not_match() {
        let a = HeaderVerificationLayer::new().unwrap();
        let version = Version::parse("9.9.0").unwrap();
        assert!(!a.version_req.matches(&version));
    }

    #[test]
    fn valid_accept_header_is_parsed() {
        let input = "application/vnd.miden.1.0.0+grpc";
        let expected = AcceptHeaderValue {
            app_name: "miden",
            version: "1.0.0",
            response_type: "grpc",
        };
        assert_eq!(AcceptHeaderValue::try_from(input), Ok(expected));
    }
}
