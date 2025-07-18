use std::{
    collections::HashMap,
    str::FromStr,
    task::{Context as StdContext, Poll},
};

use futures::{FutureExt, future::BoxFuture};
use http::{
    HeaderMap, HeaderValue,
    header::{ACCEPT, ToStrError},
};
use itertools::Itertools;
use miden_node_utils::ErrorReport;
use miden_objects::Digest;
use miden_tx::utils::HexParseError;
use nom::bytes::complete::{tag, take_until};
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
    genesis_commitment: Digest,
}

#[derive(Debug, thiserror::Error)]
enum AcceptHeaderError {
    #[error("Header value could not be parsed as a string")]
    InvalidValue(#[source] ToStrError),

    #[error("the accept header could not be parsed")]
    InvalidMediaRange(#[source] MediaRangeParsingError),

    #[error("version value {1} failed to parse")]
    VersionParsing(#[source] semver::Error, String),

    #[error("genesis value {1} failed to parse")]
    GenesisParsing(#[source] HexParseError, String),

    #[error("server does not support any of the specified application/vnd.miden content types")]
    NoSupportedMediaRange,
}

impl AcceptHeaderLayer {
    pub fn new(rpc_version: Version, genesis_commitment: Digest) -> Self {
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
    fn verify(&self, headers: &HeaderMap<HeaderValue>) -> Result<(), AcceptHeaderError> {
        let Some(header) = headers.get(ACCEPT) else {
            return Ok(());
        };

        let header = header.to_str().map_err(AcceptHeaderError::InvalidValue)?;

        let mut media_ranges = header
            .split(",")
            .map(MediaRange::from_str)
            // Ignore media types that aren't ours. Wildcards are accepted.
            .filter_ok(MediaRange::is_miden_application)
            .collect::<Result<Vec<_>, _>>()
            .map_err(AcceptHeaderError::InvalidMediaRange)?;

        // If there are no miden specific headers, then accept the request as the client has no
        // stated preference that we care about.
        if media_ranges.is_empty() {
            return Ok(());
        }

        // Find the most preferred type that we support.
        media_ranges.sort_unstable_by(|a, b| b.quality.cmp(&a.quality));
        for range in media_ranges {
            // Verify the user's version requirement against our rpc version.
            if let Some(version) = range.params.get(&CaselessKey::VERSION) {
                let requirement = VersionReq::parse(version)
                    .map_err(|err| AcceptHeaderError::VersionParsing(err, version.to_string()))?;

                if !requirement.matches(&self.rpc_version) {
                    continue;
                }
            }

            // Verify the user's genesis commitment against ours.
            if let Some(genesis) = range.params.get(&CaselessKey::GENESIS) {
                let genesis = Digest::try_from(*genesis)
                    .map_err(|err| AcceptHeaderError::GenesisParsing(err, genesis.to_string()))?;

                if genesis != self.genesis_commitment {
                    continue;
                }
            }

            return Ok(());
        }

        // No matching media type found.
        //
        // We know the client specified at least one miden specific media type at this point.
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

#[derive(Debug, thiserror::Error)]
enum MediaRangeParsingError {
    #[error("media range contained no types")]
    MissingMediaType,
    #[error("the media type {0} is invalid")]
    InvalidMediaType(String),
    #[error("the parameter {0} is invalid")]
    InvalidParameter(String),
    #[error("the quality weight value {0} is invalid")]
    InvalidQuality(String),
}

struct MediaRange<'a> {
    main: MediaType<'a>,
    subtype: MediaType<'a>,
    params: HashMap<CaselessKey<'a>, &'a str>,
    quality: Quality,
}

enum MediaType<'a> {
    Wildcard,
    Type(&'a str),
}

#[derive(Eq, Hash)]
struct CaselessKey<'a>(&'a str);

impl CaselessKey<'static> {
    const VERSION: Self = Self("version");
    const GENESIS: Self = Self("genesis");
}

impl PartialEq for CaselessKey<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(other.0)
    }
}

impl<'a> MediaType<'a> {
    fn new(value: &'a str) -> Self {
        match value.trim() {
            "*" => Self::Wildcard,
            other => Self::Type(other),
        }
    }

    fn matches(&self, value: &str) -> bool {
        match self {
            MediaType::Wildcard => true,
            MediaType::Type(media) => media.eq_ignore_ascii_case(value),
        }
    }
}

/// The value of a media-range's quality weighting parameter.
///
/// The inner f32 is guaranteed to adhere to the standard's 0.0..1.0 range. As such, it has a total
/// ordering and both [`Eq`] and [`Ord`] traits are implemented for it. Manually overriding the
/// inner value is therefore a **bad idea**.
#[derive(PartialEq, PartialOrd)]
struct Quality(f32);

impl Default for Quality {
    fn default() -> Self {
        Self(1.0)
    }
}

impl Quality {
    fn from_str(s: &str) -> Option<Self> {
        f32::from_str(s)
            .ok()
            .and_then(|val| (val >= 0.0 && val <= 1.0).then_some(Self(val)))
    }
}

impl Eq for Quality {}

impl Ord for Quality {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // SAFETY: Quality is guaranteed to never be NaN by construction.
        self.partial_cmp(other).unwrap()
    }
}

impl<'a> MediaRange<'a> {
    fn from_str(s: &'a str) -> Result<Self, MediaRangeParsingError> {
        let mut parts = s.trim().split(';');
        let Some(media) = parts.next() else {
            return Err(MediaRangeParsingError::MissingMediaType);
        };

        let Some((main, subtype)) = media.split_once('/') else {
            return Err(MediaRangeParsingError::InvalidMediaType(media.to_owned()));
        };

        let mut params = parts
            .map(|kv| {
                kv.split_once('=')
                    .map(|(k, v)| (CaselessKey(k.trim()), v.trim()))
                    .ok_or(MediaRangeParsingError::InvalidParameter(kv.to_owned()))
            })
            .collect::<Result<HashMap<_, _>, _>>()?;

        let quality = params
            .remove(&CaselessKey("q"))
            .map(|val| {
                Quality::from_str(val).ok_or(MediaRangeParsingError::InvalidQuality(val.to_owned()))
            })
            .transpose()?
            .unwrap_or_default();

        Ok(Self {
            main: MediaType::new(main),
            subtype: MediaType::new(subtype),
            params,
            quality,
        })
    }

    fn is_miden_application(&self) -> bool {
        self.main.matches("application") && self.subtype.matches("vnd.miden")
    }
}

// HEADER VERIFICATION TESTS
// ================================================================================================

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderName, HeaderValue};
    use miden_objects::Digest;
    use semver::Version;

    use super::AcceptHeaderLayer;

    const TEST_GENESIS_COMMITMENT: &str =
        "0x00000000000000000000000000000000000000000000000000000000deadbeef";
    const TEST_RPC_VERSION: Version = Version::new(1, 2, 3);

    impl AcceptHeaderLayer {
        fn for_tests() -> Self {
            Self::new(TEST_RPC_VERSION, Digest::try_from(TEST_GENESIS_COMMITMENT).unwrap())
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

        let uut = AcceptHeaderLayer::for_tests();
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

        let uut = AcceptHeaderLayer::for_tests();
        uut.verify(&headers).unwrap_err();
    }
}
