use std::task::{Context as StdContext, Poll};

use futures::{FutureExt, future::BoxFuture};
use http::header::ACCEPT;
use nom::{
    IResult,
    bytes::complete::{tag, take_until},
};
use semver::{Version, VersionReq};
use tower::{Layer, Service};
use tracing::debug;

use crate::COMPONENT;

/// Layer responsible for handling HTTP ACCEPT headers.
#[derive(Clone)]
pub struct AcceptLayer {
    version_req: VersionReq,
}

impl AcceptLayer {
    /// Create a new accept layer that validates version values
    /// specified in the HTTP ACCEPT header.
    ///
    /// The version requirement is based on the version field found
    /// in the workspace's Cargo.toml file.
    ///
    /// # Panics:
    ///
    /// Panics if the version string in Cargo.toml is not valid semver.
    /// The version string is made into an env var at compile time which means
    /// that the unit tests prove this cannot panic in practice.
    pub fn new() -> anyhow::Result<Self> {
        // Parse the full version string (e.g., "0.8.0").
        let version = env!("CARGO_PKG_VERSION");
        let version = Version::parse(version)?;

        // Create version requirement string in the format ">= major.minor"
        let version_req = format!(">= {}.{}", version.major, version.minor);
        let version_req = VersionReq::parse(&version_req).expect("");
        Ok(AcceptLayer { version_req })
    }
}

impl<S> Layer<S> for AcceptLayer {
    type Service = AcceptService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        AcceptService { inner, version: self.version_req.clone() }
    }
}

/// Service responsible for handling HTTP ACCEPT headers.
#[derive(Clone)]
pub struct AcceptService<S> {
    inner: S,
    version: VersionReq,
}

impl<S, B> Service<http::Request<B>> for AcceptService<S>
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
    /// The version specified in the value of the ACCEPT header must match
    /// the version requirements specified by the server.
    fn call(&mut self, request: http::Request<B>) -> Self::Future {
        if let Some(accept_header_value) = request.headers().get(ACCEPT) {
            // Convert header value to str.
            match accept_header_value.to_str() {
                // Parse the accept str.
                Ok(accept_str) => match AcceptHeaderValue::try_from(accept_str) {
                    // Parse the version.
                    Ok(accept) => match Version::parse(accept.version) {
                        // Verify the version matches configured requirements.
                        Ok(version) => {
                            if self.version.matches(&version) {
                                self.inner.call(request).boxed()
                            } else {
                                debug!(target: COMPONENT, "Version does not match ({}/{})", version, self.version);
                                bad_request("Client / server version mismatch").boxed()
                            }
                        },
                        Err(e) => {
                            debug!(target: COMPONENT, "Failed to parse version: {}", e);
                            bad_request("Invalid version specified in accept header value").boxed()
                        },
                    },
                    Err(e) => {
                        debug!(target: COMPONENT, "Failed to parse accept header value: {}", e);
                        bad_request("Invalid accept header value").boxed()
                    },
                },
                Err(e) => {
                    debug!(target: COMPONENT, "Failed to stringify accept header value: {}", e);
                    bad_request("Invalid accept header value").boxed()
                },
            }
        } else {
            debug!(target: COMPONENT, "Request missing ACCEPT header");
            bad_request("Missing required ACCEPT header").boxed()
        }
    }
}

/// Returns a future that resolves to a bad request response.
fn bad_request<B: Default + Send + 'static, E: Send + 'static>(
    msg: &'static str,
) -> impl Future<Output = Result<http::Response<B>, E>> {
    let response = http::Response::builder()
        .status(http::StatusCode::BAD_REQUEST)
        .header("content-type", "application/grpc")
        .header("grpc-status", "3") // INVALID_ARGUMENT
        .header("grpc-message", msg)
        .body(B::default())
        .expect("headers are valid");

    futures::future::ready(Ok(response))
}

/// The result of parsing the following canonical representation
/// of an ACCEPT header value: `application/vnd.{app_name}.{version}+{response_type}`.
#[derive(Debug, PartialEq)]
pub struct AcceptHeaderValue<'a> {
    app_name: &'a str,
    version: &'a str,
    response_type: &'a str,
}

impl AcceptHeaderValue<'_> {
    /// Parses the given input string into an [`AcceptHeaderValue`].
    fn parse(input: &str) -> IResult<&str, AcceptHeaderValue> {
        let (input, _) = tag("application/vnd.")(input)?;
        let (input, app_name) = take_until(".")(input)?;
        let (input, _) = tag(".")(input)?;
        let (input, version) = take_until("+")(input)?;
        let (response_type, _) = tag("+")(input)?;

        Ok(("", AcceptHeaderValue { app_name, version, response_type }))
    }
}

impl<'a> TryFrom<&'a str> for AcceptHeaderValue<'a> {
    type Error = nom::Err<nom::error::Error<&'a str>>;

    fn try_from(input: &'a str) -> Result<Self, Self::Error> {
        Self::parse(input).map(|(_, value)| value)
    }
}

// ACCEPT TESTS
// ================================================================================================

#[cfg(test)]
mod tests {
    use semver::Version;

    use super::{AcceptHeaderValue, AcceptLayer};

    #[test]
    fn current_version_is_parsed_and_matches() {
        let a = AcceptLayer::new().unwrap();
        let full_version = env!("CARGO_PKG_VERSION");
        let full_version = Version::parse(full_version).unwrap();
        assert!(a.version_req.matches(&full_version));
    }

    #[test]
    fn valid_accept_header_is_parsed() {
        let input = "application/vnd.miden.v1.0.0+grpc";
        let expected = AcceptHeaderValue {
            app_name: "miden",
            version: "v1.0.0",
            response_type: "grpc",
        };
        assert_eq!(AcceptHeaderValue::try_from(input), Ok(expected));
    }
}
