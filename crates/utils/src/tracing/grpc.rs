/// Represents an Otel compatible, traced Miden component.
///
/// Used to select an appropriate service name for the tonic server to use.
#[derive(Copy, Clone)]
pub enum TracedComponent {
    Rpc,
    BlockProducer,
    StoreRpc,
    StoreBlockProducer,
    StoreNtxBuilder,
    RemoteProver,
    RemoteProverProxy,
}

impl TracedComponent {
    /// Returns the service name for this component.
    fn service_name(self) -> &'static str {
        match self {
            TracedComponent::Rpc => "rpc.rpc",
            TracedComponent::BlockProducer => "block-producer.rpc",
            TracedComponent::StoreRpc => "store.rpc.rpc",
            TracedComponent::StoreBlockProducer => "store.block-producer.rpc",
            TracedComponent::StoreNtxBuilder => "store.ntx-builder.rpc",
            TracedComponent::RemoteProver => "remote-prover.rpc",
            TracedComponent::RemoteProverProxy => "remote-prover-proxy.rpc",
        }
    }
}

/// Returns a [`trace_fn`](tonic::transport::server::Server) implementation for the RPC which
/// adds open-telemetry information to the span.
///
/// Creates an `info` span following the open-telemetry standard: `{service}.rpc/{method}`.
/// The span name is dynamically set using the HTTP path via the `otel.name` field.
/// Additionally also pulls in remote tracing context which allows the server trace to be connected
/// to the client's origin trace.
pub fn traced_span_fn<T>(component: TracedComponent) -> fn(&http::Request<T>) -> tracing::Span {
    match component {
        TracedComponent::Rpc => rpc_trace_fn,
        TracedComponent::BlockProducer => block_producer_trace_fn,
        TracedComponent::StoreRpc => store_rpc_trace_fn,
        TracedComponent::StoreBlockProducer => store_block_producer_trace_fn,
        TracedComponent::StoreNtxBuilder => store_ntx_builder_trace_fn,
        TracedComponent::RemoteProver => remote_prover_trace_fn,
        TracedComponent::RemoteProverProxy => remote_prover_proxy_trace_fn,
    }
}

/// Creates a dynamic tracing span based on the HTTP path of the gRPC request.
///
/// This function extracts the method name from the HTTP path and creates a span with
/// the service name from the component. The actual span name used by OpenTelemetry
/// is set dynamically via the `otel.name` field, allowing us to avoid hardcoded
/// method name matches.
fn dynamic_trace_fn<T>(request: &http::Request<T>, component: TracedComponent) -> tracing::Span {
    let service_name = component.service_name();

    // Extract the method name from the HTTP path (e.g., "/rpc.rpc/CheckNullifiers" ->
    // "CheckNullifiers")
    let method_name = request.uri().path().rsplit('/').next().unwrap_or("Unknown");

    // Create a span with a generic name - the actual span name will be set via otel.name
    let span =
        tracing::info_span!("grpc_request", rpc.service = service_name, rpc.method = method_name);

    // Set the dynamic span name that OpenTelemetry will use
    // This allows us to have the proper <service>/<method> format without hardcoding
    span.record("otel.name", format!("{service_name}/{method_name}"));

    // Add OpenTelemetry context and network attributes
    let span = add_otel_span_attributes(span, request);
    add_network_attributes(span, request)
}

// Individual trace functions that delegate to the dynamic implementation
fn rpc_trace_fn<T>(request: &http::Request<T>) -> tracing::Span {
    dynamic_trace_fn(request, TracedComponent::Rpc)
}

fn block_producer_trace_fn<T>(request: &http::Request<T>) -> tracing::Span {
    dynamic_trace_fn(request, TracedComponent::BlockProducer)
}

fn store_rpc_trace_fn<T>(request: &http::Request<T>) -> tracing::Span {
    dynamic_trace_fn(request, TracedComponent::StoreRpc)
}

fn store_block_producer_trace_fn<T>(request: &http::Request<T>) -> tracing::Span {
    dynamic_trace_fn(request, TracedComponent::StoreBlockProducer)
}

fn store_ntx_builder_trace_fn<T>(request: &http::Request<T>) -> tracing::Span {
    dynamic_trace_fn(request, TracedComponent::StoreNtxBuilder)
}

fn remote_prover_trace_fn<T>(request: &http::Request<T>) -> tracing::Span {
    dynamic_trace_fn(request, TracedComponent::RemoteProver)
}

fn remote_prover_proxy_trace_fn<T>(request: &http::Request<T>) -> tracing::Span {
    dynamic_trace_fn(request, TracedComponent::RemoteProverProxy)
}

/// Adds remote tracing context to the span.
///
/// Could be expanded in the future by adding in more open-telemetry properties.
fn add_otel_span_attributes<T>(span: tracing::Span, request: &http::Request<T>) -> tracing::Span {
    // Pull the open-telemetry parent context using the HTTP extractor. We could make a more
    // generic gRPC extractor by utilising the gRPC metadata. However that
    //     (a) requires cloning headers,
    //     (b) we would have to write this ourselves, and
    //     (c) gRPC metadata is transferred using HTTP headers in any case.
    let otel_ctx = opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.extract(&MetadataExtractor(&tonic::metadata::MetadataMap::from_headers(
            request.headers().clone(),
        )))
    });
    tracing_opentelemetry::OpenTelemetrySpanExt::set_parent(&span, otel_ctx);

    span
}

/// Adds various network attributes to the span, including remote address and port.
///
/// See [server attributes](https://opentelemetry.io/docs/specs/semconv/rpc/rpc-spans/#server-attributes).
fn add_network_attributes<T>(span: tracing::Span, request: &http::Request<T>) -> tracing::Span {
    use super::OpenTelemetrySpanExt;
    // Set HTTP attributes.
    span.set_attribute("rpc.system", "grpc");
    if let Some(host) = request.uri().host() {
        span.set_attribute("server.address", host);
    }
    if let Some(host_port) = request.uri().port() {
        span.set_attribute("server.port", host_port.as_u16());
    }
    let remote_addr = request
        .extensions()
        .get::<tonic::transport::server::TcpConnectInfo>()
        .and_then(tonic::transport::server::TcpConnectInfo::remote_addr);
    if let Some(addr) = remote_addr {
        span.set_attribute("client.address", addr.ip());
        span.set_attribute("client.port", addr.port());
        span.set_attribute("network.peer.address", addr.ip());
        span.set_attribute("network.peer.port", addr.port());
        span.set_attribute("network.transport", "tcp");
        match addr.ip() {
            std::net::IpAddr::V4(_) => span.set_attribute("network.type", "ipv4"),
            std::net::IpAddr::V6(_) => span.set_attribute("network.type", "ipv6"),
        }
    }

    span
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
