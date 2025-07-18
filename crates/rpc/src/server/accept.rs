use std::{
    collections::HashMap,
    ops::Not,
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

        // Whether an empty is valid or not is up for debate, for ease of use lets just accept it.
        if header.is_empty() {
            return Ok(());
        }

        let mut media_ranges = header
            .split(",")
            // Allow trailing comma's.
            .filter_map(|range| {
                let range = range.trim();
                range.is_empty().not().then_some(range)
            })
            .map(MediaRange::from_str)
            // Ignore media types that aren't ours. Wildcards are accepted, as are obsolete
            // legacy values: application/vnd.miden+grpc.
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
            // If weighting is zero then this is not desired by the client.
            if range.quality.0 == 0.0 {
                continue;
            }

            // Skip obsolete variants.
            if range.is_obsolete() {
                continue;
            }

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

    fn starts_with(&self, prefix: &str) -> bool {
        match self {
            MediaType::Wildcard => false,
            MediaType::Type(media) => {
                media[..prefix.len().min(media.len())].eq_ignore_ascii_case(prefix)
            },
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
            // Handle a trailing semi-comma which otherwise results in an "empty" parameter.
            .filter_map(|kv| {
                let kv = kv.trim();
                kv.is_empty().not().then_some(kv)
            })
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
        // The subtype checks prefix only to include the legacy `vnd.miden+grpc` and optional
        // version.
        self.main.matches("application")
            && (self.subtype.matches("vnd.miden") || self.subtype.starts_with("vnd.miden+grpc"))
    }

    fn is_obsolete(&self) -> bool {
        self.subtype.starts_with("vnd.miden+grpc")
    }
}

// HEADER VERIFICATION TESTS
// ================================================================================================

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue, header::ACCEPT};
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
        "application/vnd.miden; network=0x00000000000000000000000000000000000000000000000000000000deadbeef"
    )]
    #[case::matching_network_and_version(
        "application/vnd.miden; network=0x00000000000000000000000000000000000000000000000000000000deadbeef; version=1.2.3"
    )]
    #[case::parameter_order_swopped(
        "application/vnd.miden; version=1.2.3; network=0x00000000000000000000000000000000000000000000000000000000deadbeef;"
    )]
    #[case::trailing_semi_comma("application/vnd.miden; ")]
    #[case::trailing_comma("application/vnd.miden, ")]
    // This should pass because the 2nd option is valid.
    #[case::multiple_types("application/vnd.miden; version=2, application/vnd.miden")]
    #[test]
    fn request_should_pass(#[case] accept: &'static str) {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static(accept));

        let uut = AcceptHeaderLayer::for_tests();
        uut.verify(&headers).unwrap();
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
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static(accept));

        let uut = AcceptHeaderLayer::for_tests();
        uut.verify(&headers).unwrap_err();
    }
}
