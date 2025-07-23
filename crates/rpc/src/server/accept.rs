use std::{
    str::FromStr,
    task::{Context as StdContext, Poll},
};

use futures::{FutureExt, future::BoxFuture};
use http::header::{ACCEPT, ToStrError};
use mediatype::{Name, ReadParams};
use miden_node_utils::{ErrorReport, FlattenResult};
use miden_objects::{Word, WordError};
use semver::{Version, VersionReq};
use tower::{Layer, Service};

/// Performs content negotiation by rejecting requests which don't match our RPC version or network.
/// Clients can specify these as parameters in our `application/vnd.miden` accept media range.
///
/// The client can specify RPC versions it supports using the [`VersionReq`] format. The network
/// is specified as the genesis block's commitment. If the server cannot satisfy either of these
/// constraints then the request is rejected.
///
/// Note that both values are optional, as is the header itself. If unset, the server considers
/// any value acceptable.
///
/// As part of the accept header's standard, all media ranges are examined in quality weighting
/// order until a matching content type is found. This means that the client can set multiple
/// `application/vnd.miden` values and each will be tested until one passes. If none pass, the
/// request is rejected.
///
/// ## Format
///
/// Parameters are optional and order is not important.
///
/// ```
/// application/vnd.miden; version=<version-req>; genesis=0x1234
/// ```
#[derive(Clone)]
pub struct AcceptHeaderLayer {
    rpc_version: Version,
    genesis_commitment: Word,
}

#[derive(Debug, thiserror::Error)]
enum AcceptHeaderError {
    #[error("header value could not be parsed as a UTF8 string")]
    InvalidUtf8(#[source] ToStrError),

    #[error("accept header's media type could not be parsed")]
    InvalidMediaType(#[source] mediatype::MediaTypeError),

    #[error("a Q value was invalid")]
    InvalidQValue(#[source] QParsingError),

    #[error("version value failed to parse")]
    InvalidVersion(#[source] semver::Error),

    #[error("genesis value failed to parse")]
    InvalidGenesis(#[source] WordError),

    #[error("server does not support any of the specified application/vnd.miden content types")]
    NoSupportedMediaRange,
}

impl AcceptHeaderLayer {
    pub fn new(rpc_version: Version, genesis_commitment: Word) -> Self {
        AcceptHeaderLayer { rpc_version, genesis_commitment }
    }
}

impl<S> Layer<S> for AcceptHeaderLayer {
    type Service = AcceptHeaderService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        AcceptHeaderService { inner, verifier: self.clone() }
    }
}

impl AcceptHeaderLayer {
    const VERSION: Name<'static> = Name::new_unchecked("version");
    const GENESIS: Name<'static> = Name::new_unchecked("genesis");

    fn verify(&self, accept: &str) -> Result<(), AcceptHeaderError> {
        let mut media_types = mediatype::MediaTypeList::new(accept).peekable();

        // Its debateable whether an empty header value is valid. Let's err on the side of being
        // gracious if the client want's to be weird.
        if media_types.peek().is_none() {
            return Ok(());
        }

        while let Some(media_type) = media_types.next() {
            let media_type = media_type.map_err(AcceptHeaderError::InvalidMediaType)?;

            // Skip types that don't match `application/vnd.miden`.
            //
            // Note that `application/*` is invalid so we cannot collapse the conditions.
            match (media_type.ty.as_str(), media_type.subty.as_str()) {
                ("*", "*") | ("*" | "application", "vnd.miden") => {},
                _ => continue,
            }

            // Quality value may be set to zero, indicating that the client _does not_ want this
            // media type. So we skip those.
            let quality = media_type
                .get_param(mediatype::names::Q)
                .map(|value| QValue::from_str(value.as_str()))
                .transpose()
                .map_err(AcceptHeaderError::InvalidQValue)?
                .unwrap_or_default();

            if quality.is_zero() {
                continue;
            }

            // Skip those that don't match the version requirement.
            let version = media_type
                .get_param(Self::VERSION)
                .map(|value| VersionReq::parse(value.as_str()))
                .transpose()
                .map_err(AcceptHeaderError::InvalidVersion)?;
            if !version.is_some_and(|v| v.matches(&self.rpc_version)) {
                continue;
            }

            // Skip if the genesis commitment does not match.
            let genesis = media_type
                .get_param(Self::GENESIS)
                .map(|value| Word::try_from(value.as_str()))
                .transpose()
                .map_err(AcceptHeaderError::InvalidGenesis)?;
            if genesis.is_some_and(|g| g != self.genesis_commitment) {
                continue;
            }

            // All preconditions met, this is a valid media type that we can serve.
            return Ok(());
        }

        // We've already handled the case where there are no media types specified, so if we are
        // here its because the client _did_ specify some but none of them are a match.
        Err(AcceptHeaderError::NoSupportedMediaRange)
    }
}

/// Service responsible for handling HTTP ACCEPT headers.
#[derive(Clone)]
pub struct AcceptHeaderService<S> {
    inner: S,
    verifier: AcceptHeaderLayer,
}

impl<S, B> Service<http::Request<B>> for AcceptHeaderService<S>
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
        let Some(header) = request.headers().get(ACCEPT) else {
            return self.inner.call(request).boxed();
        };

        let result = header
            .to_str()
            .map_err(AcceptHeaderError::InvalidUtf8)
            .map(|header| self.verifier.verify(header))
            .flatten_result();

        match result {
            Ok(()) => self.inner.call(request).boxed(),
            Err(err) => {
                let response = tonic::Status::invalid_argument(err.as_report()).into_http();

                futures::future::ready(Ok(response)).boxed()
            },
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum QParsingError {
    #[error("Q value contained too many decimal digits")]
    TooManyDigits,
    #[error("invalid decimal digit {0}")]
    InvalidDigit(u8),
    #[error("invalid format")]
    BadFormat,
}

/// Denotes the value of the `Q` parameter which indicates priority of the media-type.
///
/// Has a range of 0..=1 and can have upto three decimals.
struct QValue {
    /// Represents the range 0..=1 with three possible decimal digits by multiplying the original
    /// fraction by 1000.
    kilo: u16,
}

/// As per spec, the default value is 1 if unspecified.
impl Default for QValue {
    fn default() -> Self {
        Self { kilo: 1000 }
    }
}

impl QValue {
    fn is_zero(&self) -> bool {
        self.kilo == 0
    }
}

impl FromStr for QValue {
    type Err = QParsingError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let kilo = match s.as_bytes() {
            [b'1'] => 1000,
            [b'1', b'.', rest @ ..] if rest.iter().all(|&c| c == b'0') => 1000,
            [b'0'] => 0,
            [b'0', b'.', rest @ ..] => {
                if rest.len() > 3 {
                    return Err(Self::Err::TooManyDigits);
                }

                let mut value = 0u16;
                for digit in rest {
                    match digit {
                        b'0'..b'1' => value = value * 10 + u16::from(digit - b'0'),
                        invalid => return Err(Self::Err::InvalidDigit(*invalid)),
                    }
                }

                value
            },
            _ => return Err(Self::Err::BadFormat),
        };

        Ok(Self { kilo })
    }
}

// HEADER VERIFICATION TESTS
// ================================================================================================

#[cfg(test)]
mod tests {
    use miden_objects::Word;
    use semver::Version;

    use super::AcceptHeaderLayer;

    const TEST_GENESIS_COMMITMENT: &str =
        "0x00000000000000000000000000000000000000000000000000000000deadbeef";
    const TEST_RPC_VERSION: Version = Version::new(1, 2, 3);

    impl AcceptHeaderLayer {
        fn for_tests() -> Self {
            Self::new(TEST_RPC_VERSION, Word::try_from(TEST_GENESIS_COMMITMENT).unwrap())
        }
    }

    #[rstest::rstest]
    #[case::empty("")]
    #[case::wildcard("*/*")]
    #[case::wildcard_subtype("application/*")]
    #[case::media_type_only("application/vnd.miden")]
    #[case::with_quality("application/vnd.miden; q=0.3")]
    #[case::version_exact("application/vnd.miden; version==1.2.3")]
    #[case::version_major("application/vnd.miden; version=1")]
    #[case::version_minor("application/vnd.miden; version=1.2")]
    #[case::version_wildcard_major("application/vnd.miden; version=*")]
    #[case::version_wildcard_minor("application/vnd.miden; version=1.*")]
    #[case::version_wildcard_patch("application/vnd.miden; version=1.2.*")]
    #[case::matching_network(
        "application/vnd.miden; genesis=0x00000000000000000000000000000000000000000000000000000000deadbeef"
    )]
    #[case::matching_network_and_version(
        "application/vnd.miden; genesis=0x00000000000000000000000000000000000000000000000000000000deadbeef; version=1.2.3"
    )]
    #[case::parameter_order_swopped(
        "application/vnd.miden; version=1.2.3; genesis=0x00000000000000000000000000000000000000000000000000000000deadbeef;"
    )]
    #[case::trailing_semi_comma("application/vnd.miden; ")]
    #[case::trailing_comma("application/vnd.miden, ")]
    #[case::trailing_commas(", , , ,   , , application/vnd.miden, ")]
    // This should pass because the 2nd option is valid.
    #[case::multiple_types("application/vnd.miden; version=2, application/vnd.miden")]
    #[case::whitespace_agnostic(
        "   application/vnd.miden  ;        genesis = 0x00000000000000000000000000000000000000000000000000000000deadbeef ;        version = 1.2.3"
    )]
    #[test]
    fn request_should_pass(#[case] accept: &'static str) {
        AcceptHeaderLayer::for_tests().verify(accept).unwrap();
    }

    #[rstest::rstest]
    #[case::backwards_incompatible_with_version("application/vnd.miden+grpc.1.2.3")]
    #[case::backwards_incompatible("application/vnd.miden+grpc")]
    #[case::invalid_version("application/vnd.miden; version=0x123")]
    #[case::invalid_genesis("application/vnd.miden; genesis=aaa")]
    #[case::version_too_old("application/vnd.miden; version=>1.2.3")]
    #[case::version_too_new("application/vnd.miden; version=<1.2.3")]
    #[case::zero_weighting("application/vnd.miden; q=0.0")]
    #[test]
    fn request_should_be_rejected(#[case] accept: &'static str) {
        AcceptHeaderLayer::for_tests().verify(accept).unwrap_err();
    }
}
