use std::time::Duration;

use http::header::HeaderName;
use tower_governor::key_extractor::{KeyExtractor, SmartIpKeyExtractor};
use tower_http::classify::{GrpcCode, GrpcErrorsAsFailures, GrpcFailureClass, SharedClassifier};
use tower_http::trace::{DefaultOnBodyChunk, DefaultOnRequest, TraceLayer};
use tracing::field;

use super::ErrorSpanExt;

/// The span field holding the numeric gRPC status code of the response, following the
/// [OTel RPC semantic conventions](https://opentelemetry.io/docs/specs/semconv/rpc/grpc/).
///
/// Always recorded on request root spans: `0` (OK) on success, the actual code on failure. This
/// lets queries distinguish request outcomes without relying on the span's error status, which is
/// reserved for node faults (see [`is_server_fault_code`]).
const GRPC_STATUS_CODE_FIELD: &str = "rpc.grpc.status_code";

/// Returns a [`trace_fn`](tonic::transport::server::Server) implementation for gRPC requests
/// which adds open-telemetry information to the span.
///
/// Creates an `info` span following the open-telemetry standard: `{service}/{method}`.
/// The span name is dynamically set using the HTTP path via the `otel.name` field.
/// Additionally also pulls in remote tracing context which allows the server trace to be connected
/// to the client's origin trace.
#[track_caller]
pub fn grpc_trace_fn<T>(request: &http::Request<T>) -> tracing::Span {
    // A gRPC request's path ends with `../<service>/<method>`.
    let mut path_segments = request.uri().path().rsplit('/');

    let method = path_segments.next().unwrap_or_default();
    let service = path_segments.next().unwrap_or_default();

    // Create a span with a generic, static name. Fields to be recorded after needs to be
    // initialized as empty since otherwise the assignment will have no effect.
    let span = tracing::info_span!(
        "rpc",
        otel.name = field::Empty,
        rpc.service = service,
        rpc.method = method,
        rpc.system = field::Empty,
        rpc.request.size = field::Empty,
        rpc.response.size = field::Empty,
        rpc.grpc.status_code = field::Empty,
        server.address = field::Empty,
        server.port = field::Empty,
        client.address = field::Empty,
        client.port = field::Empty,
        network.peer.address = field::Empty,
        network.peer.port = field::Empty,
        network.transport = field::Empty,
        network.type = field::Empty,
    );

    // Set the span name via otel.name
    let otel_name = format!("{service}/{method}");
    span.record("otel.name", otel_name);

    // Pull the open-telemetry parent context using the HTTP extractor
    let otel_ctx = opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.extract(&MetadataExtractor(&tonic::metadata::MetadataMap::from_headers(
            request.headers().clone(),
        )))
    });
    let _ = tracing_opentelemetry::OpenTelemetrySpanExt::set_parent(&span, otel_ctx);

    // Adds various network attributes to the span, including remote address and port.
    //
    // See [server attributes](https://opentelemetry.io/docs/specs/semconv/rpc/rpc-spans/#server-attributes).

    // Set HTTP attributes.
    span.record("rpc.system", "grpc");
    if let Some(host) = request.uri().host() {
        span.record("server.address", host);
    }
    if let Some(host_port) = request.uri().port() {
        span.record("server.port", host_port.as_u16());
    }
    let remote_addr = request
        .extensions()
        .get::<tonic::transport::server::TcpConnectInfo>()
        .and_then(tonic::transport::server::TcpConnectInfo::remote_addr);

    // client.address should be the resolved IP address of the client, if available. In the case of
    // a reverse proxy, this may not be the same as the remote address.
    if let Ok(ip) = SmartIpKeyExtractor.extract(request) {
        span.record("client.address", field::display(ip));
    } else if let Some(addr) = remote_addr {
        span.record("client.address", field::display(addr.ip()));
        span.record("client.port", addr.port());
    }

    if let Some(addr) = remote_addr {
        span.record("network.peer.address", field::display(addr.ip()));
        span.record("network.peer.port", addr.port());
        span.record("network.transport", "tcp");
        match addr.ip() {
            std::net::IpAddr::V4(_) => span.record("network.type", "ipv4"),
            std::net::IpAddr::V6(_) => span.record("network.type", "ipv6"),
        };
    }

    for header in [
        http::header::ACCEPT,
        http::header::ORIGIN,
        http::header::USER_AGENT,
        http::header::FORWARDED,
        HeaderName::from_static("x-forwarded-for"),
        HeaderName::from_static("x-real-ip"),
        HeaderName::from_static("x-request-id"),
    ] {
        if let Some(value) = request.headers().get(&header) {
            if let Ok(value) = value.to_str() {
                tracing_opentelemetry::OpenTelemetrySpanExt::set_attribute(
                    &span,
                    format!("http.request.header.{header}"),
                    value.to_owned(),
                );
            }
        }
    }

    span
}

/// Returns whether a gRPC status code indicates a fault in the node, as opposed to a failure
/// caused by the request itself.
///
/// Client-caused failures (invalid arguments, failed preconditions, exhausted quotas, ...) are
/// *successful rejections* and must not mark spans with `OTel` error status, otherwise client noise
/// becomes indistinguishable from node failures in error-based alerting.
///
/// The set mirrors the [OTel gRPC semantic conventions] for server spans, except that
/// `UNIMPLEMENTED` is treated as client-caused: on a public API, calls to unknown methods are
/// client noise, not a node fault.
///
/// [OTel gRPC semantic conventions]: https://opentelemetry.io/docs/specs/semconv/rpc/grpc/
pub fn is_server_fault_code(code: tonic::Code) -> bool {
    matches!(
        code,
        tonic::Code::Unknown
            | tonic::Code::DeadlineExceeded
            | tonic::Code::Internal
            | tonic::Code::Unavailable
            | tonic::Code::DataLoss
    )
}

/// Classifies errors into node faults vs client-caused failures for telemetry purposes.
///
/// Implemented by [`tonic::Status`] and by error enums deriving
/// `miden_node_proto::errors::GrpcError`.
pub trait GrpcFault {
    /// Returns whether this error indicates a fault in the node rather than a bad request.
    fn is_server_fault(&self) -> bool;
}

impl GrpcFault for tonic::Status {
    fn is_server_fault(&self) -> bool {
        is_server_fault_code(self.code())
    }
}

/// Records a request-handling error on the current span.
///
/// Called by `miden_instrument`'s `grpc_err` directive. Node faults are logged at error level and
/// mark the span with `OTel` error status, matching the plain `err` directive; client-caused
/// failures are logged at debug level and leave the span status untouched, since rejecting a bad
/// request is the node behaving correctly.
pub fn record_grpc_error<E>(err: &E)
where
    E: GrpcFault + std::error::Error,
{
    use crate::ErrorReport;

    if err.is_server_fault() {
        tracing::error!(error = err.as_report());
        tracing::Span::current().set_error(err);
    } else {
        tracing::debug!(error = err.as_report());
    }
}

/// Returns the [`TraceLayer`] for gRPC servers.
///
/// - Creates the per-request root span via [`grpc_trace_fn`].
/// - Records `rpc.grpc.status_code` on every request span — `0` (OK) on success, the actual code
///   on failure — so request outcomes are always queryable.
/// - Marks the span with `OTel` error status only for codes indicating a node fault (see
///   [`is_server_fault_code`]); client-caused failures keep the span status unset.
pub fn grpc_trace_layer() -> TraceLayer<
    SharedClassifier<GrpcErrorsAsFailures>,
    GrpcMakeSpan,
    DefaultOnRequest,
    GrpcOnResponse,
    DefaultOnBodyChunk,
    GrpcOnEos,
    GrpcOnFailure,
> {
    TraceLayer::new(SharedClassifier::new(grpc_fault_classifier()))
        .make_span_with(GrpcMakeSpan)
        .on_response(GrpcOnResponse)
        .on_eos(GrpcOnEos)
        .on_failure(GrpcOnFailure)
}

/// Returns the response classifier used by [`grpc_trace_layer`].
///
/// The complement of [`is_server_fault_code`]: client-caused codes are classified as successes so
/// they never reach [`GrpcOnFailure`].
fn grpc_fault_classifier() -> GrpcErrorsAsFailures {
    GrpcErrorsAsFailures::new()
        .with_success(GrpcCode::Cancelled)
        .with_success(GrpcCode::InvalidArgument)
        .with_success(GrpcCode::NotFound)
        .with_success(GrpcCode::AlreadyExists)
        .with_success(GrpcCode::PermissionDenied)
        .with_success(GrpcCode::ResourceExhausted)
        .with_success(GrpcCode::FailedPrecondition)
        .with_success(GrpcCode::Aborted)
        .with_success(GrpcCode::OutOfRange)
        .with_success(GrpcCode::Unimplemented)
        .with_success(GrpcCode::Unauthenticated)
}

/// [`tower_http::trace::MakeSpan`] implementation wrapping [`grpc_trace_fn`].
#[derive(Clone, Copy, Debug)]
pub struct GrpcMakeSpan;

impl<B> tower_http::trace::MakeSpan<B> for GrpcMakeSpan {
    fn make_span(&mut self, request: &http::Request<B>) -> tracing::Span {
        grpc_trace_fn(request)
    }
}

/// Records the gRPC status code carried in the response headers (tonic sends error statuses as
/// "trailers-only" responses, i.e. in the headers).
///
/// When the header is absent the status arrives in the trailers instead; `OK` is recorded
/// provisionally so the field is always present, and [`GrpcOnEos`] / [`GrpcOnFailure`] overwrite
/// it with the actual code once known.
#[derive(Clone, Copy, Debug)]
pub struct GrpcOnResponse;

impl<B> tower_http::trace::OnResponse<B> for GrpcOnResponse {
    fn on_response(self, response: &http::Response<B>, _latency: Duration, span: &tracing::Span) {
        let code = grpc_status_from_headers(response.headers()).unwrap_or(0);
        span.record(GRPC_STATUS_CODE_FIELD, code);
    }
}

/// Records the gRPC status code from the response trailers at end-of-stream.
#[derive(Clone, Copy, Debug)]
pub struct GrpcOnEos;

impl tower_http::trace::OnEos for GrpcOnEos {
    fn on_eos(
        self,
        trailers: Option<&http::HeaderMap>,
        _stream_duration: Duration,
        span: &tracing::Span,
    ) {
        if let Some(code) = trailers.and_then(grpc_status_from_headers) {
            span.record(GRPC_STATUS_CODE_FIELD, code);
        }
    }
}

/// Marks the request span as failed.
///
/// Only invoked for classifications indicating a node fault (see [`grpc_trace_layer`]);
/// client-caused failures never reach this.
#[derive(Clone, Debug)]
pub struct GrpcOnFailure;

impl tower_http::trace::OnFailure<GrpcFailureClass> for GrpcOnFailure {
    fn on_failure(
        &mut self,
        classification: GrpcFailureClass,
        latency: Duration,
        span: &tracing::Span,
    ) {
        let code = match &classification {
            GrpcFailureClass::Code(code) => code.get(),
            // Transport-level failure without a gRPC status; map to `UNKNOWN`.
            GrpcFailureClass::Error(_) => tonic::Code::Unknown as i32,
        };
        span.record(GRPC_STATUS_CODE_FIELD, code);
        tracing_opentelemetry::OpenTelemetrySpanExt::set_status(
            span,
            opentelemetry::trace::Status::Error {
                description: classification.to_string().into(),
            },
        );
        tracing::error!(classification = %classification, latency = ?latency, "request failed");
    }
}

/// Parses the numeric `grpc-status` code from a header or trailer map.
fn grpc_status_from_headers(headers: &http::HeaderMap) -> Option<i32> {
    headers.get("grpc-status")?.to_str().ok()?.parse().ok()
}

/// Injects open-telemetry remote context into traces.
#[derive(Copy, Clone)]
pub struct OtelInterceptor;

impl tonic::service::Interceptor for OtelInterceptor {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> Result<tonic::Request<()>, tonic::Status> {
        use tracing_opentelemetry::OpenTelemetrySpanExt;
        let ctx = tracing::Span::current().context();
        opentelemetry::global::get_text_map_propagator(|propagator| {
            propagator.inject_context(&ctx, &mut MetadataInjector(request.metadata_mut()));
        });

        Ok(request)
    }
}

struct MetadataExtractor<'a>(&'a tonic::metadata::MetadataMap);
impl opentelemetry::propagation::Extractor for MetadataExtractor<'_> {
    /// Get a value for a key from the `MetadataMap`.  If the value can't be converted to &str,
    /// returns None
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|metadata| metadata.to_str().ok())
    }

    /// Collect all the keys from the `MetadataMap`.
    fn keys(&self) -> Vec<&str> {
        self.0
            .keys()
            .map(|key| match key {
                tonic::metadata::KeyRef::Ascii(v) => v.as_str(),
                tonic::metadata::KeyRef::Binary(v) => v.as_str(),
            })
            .collect::<Vec<_>>()
    }
}

struct MetadataInjector<'a>(&'a mut tonic::metadata::MetadataMap);
impl opentelemetry::propagation::Injector for MetadataInjector<'_> {
    /// Set a key and value in the `MetadataMap`.  Does nothing if the key or value are not valid
    /// inputs
    fn set(&mut self, key: &str, value: String) {
        if let Ok(key) = tonic::metadata::MetadataKey::from_bytes(key.as_bytes())
            && let Ok(val) = tonic::metadata::MetadataValue::try_from(&value)
        {
            self.0.insert(key, val);
        }
    }
}

#[cfg(test)]
mod tests {
    use tower_http::classify::{ClassifiedResponse, ClassifyResponse};

    use super::*;

    #[test]
    fn parses_grpc_status_header() {
        let mut headers = http::HeaderMap::new();
        assert_eq!(grpc_status_from_headers(&headers), None);

        headers.insert("grpc-status", "3".parse().unwrap());
        assert_eq!(grpc_status_from_headers(&headers), Some(3));
    }

    /// The trace layer's classifier decides which responses mark the request span as an error; it
    /// must agree with [`is_server_fault_code`], which drives the same decision for handler spans
    /// via `grpc_err`.
    #[test]
    fn classifier_agrees_with_fault_classification() {
        // 0..=16 covers every gRPC status code.
        for code in 0..=16i32 {
            let response = http::Response::builder()
                .header("grpc-status", code.to_string())
                .body(())
                .unwrap();

            let classified_as_failure = matches!(
                grpc_fault_classifier().classify_response(&response),
                ClassifiedResponse::Ready(Err(_))
            );
            let is_fault = code != 0 && is_server_fault_code(tonic::Code::from(code));

            assert_eq!(classified_as_failure, is_fault, "gRPC code {code}");
        }
    }
}
