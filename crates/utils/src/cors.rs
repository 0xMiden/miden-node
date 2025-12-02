use std::time::Duration;

use http::HeaderName;
use tonic::Status;
use tower_http::cors::{AllowOrigin, CorsLayer};

// CORS headers
const DEFAULT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const DEFAULT_EXPOSED_HEADERS: [HeaderName; 3] =
    [Status::GRPC_STATUS, Status::GRPC_MESSAGE, Status::GRPC_STATUS_DETAILS];
const DEFAULT_ALLOW_HEADERS: [HeaderName; 4] = [
    HeaderName::from_static("x-grpc-web"),
    http::header::CONTENT_TYPE,
    HeaderName::from_static("x-user-agent"),
    HeaderName::from_static("grpc-timeout"),
];

/// Enables CORS support. This is required for gRPC-web support.
///
/// The following implementation is based on the one in tonic-web that was deprecated in favor of letting the user configure the CORS layer. Reference: <https://github.com/hyperium/tonic/pull/1982/files>
///
/// # Configuration
///
/// The following configuration is used:
///
/// - `allow_origin`: Mirrors the request origin.
/// - `allow_credentials`: Sets the `access-control-allow-credentials` header.
/// - `max_age`: Sets the `access-control-max-age` header to 24 hours.
/// - `expose_headers`: Sets the `access-control-expose-headers` header to the following headers:
///   - `Status::GRPC_STATUS`
///   - `Status::GRPC_MESSAGE`
///   - `Status::GRPC_STATUS_DETAILS`
/// - `allow_headers`: Sets the `access-control-allow-headers` header to the following headers:
///   - `HeaderName::from_static("x-grpc-web")`
///   - `http::header::CONTENT_TYPE`
///   - `HeaderName::from_static("x-user-agent")`
///   - `HeaderName::from_static("grpc-timeout")`
pub fn cors_for_grpc_web_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::mirror_request())
        .allow_credentials(true)
        .max_age(DEFAULT_MAX_AGE)
        .expose_headers(DEFAULT_EXPOSED_HEADERS)
        .allow_headers(DEFAULT_ALLOW_HEADERS)
}
