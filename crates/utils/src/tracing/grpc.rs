/// Creates a [`tracing::Span`] based on RPC service and method name.
macro_rules! rpc_span {
    ($service:literal, $method:literal) => {
        tracing::info_span!(
            concat!($service, "/", $method),
            rpc.service = $service,
            rpc.method = $method
        )
    };
}

/// Represents an Otel compatible, traced Miden component.
///
/// Used to select an appropriate [`tracing::Span`] for the tonic server to use.
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

/// Returns a [`trace_fn`](tonic::transport::server::Server) implementation for the RPC which
/// adds open-telemetry information to the span.
///
/// Creates an `info` span following the open-telemetry standard: `{service}.rpc/{method}`.
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

fn rpc_trace_fn<T>(request: &http::Request<T>) -> tracing::Span {
    let span = match request.uri().path().rsplit('/').next() {
        Some("CheckNullifiers") => rpc_span!("rpc.rpc", "CheckNullifiers"),
        Some("CheckNullifiersByPrefix") => rpc_span!("rpc.rpc", "CheckNullifiersByPrefix"),
        Some("GetBlockHeaderByNumber") => rpc_span!("rpc.rpc", "GetBlockHeaderByNumber"),
        Some("SyncState") => rpc_span!("rpc.rpc", "SyncState"),
        Some("SyncNotes") => rpc_span!("rpc.rpc", "SyncNotes"),
        Some("GetNotesById") => rpc_span!("rpc.rpc", "GetNotesById"),
        Some("SubmitProvenTransaction") => rpc_span!("rpc.rpc", "SubmitProvenTransaction"),
        Some("GetAccountDetails") => rpc_span!("rpc.rpc", "GetAccountDetails"),
        Some("GetBlockByNumber") => rpc_span!("rpc.rpc", "GetBlockByNumber"),
        Some("GetAccountStateDelta") => rpc_span!("rpc.rpc", "GetAccountStateDelta"),
        Some("GetAccountProofs") => rpc_span!("rpc.rpc", "GetAccountProofs"),
        Some("Status") => rpc_span!("rpc.rpc", "Status"),
        _ => rpc_span!("rpc.rpc", "Unknown"),
    };
    add_network_attributes(span, request)
}

fn block_producer_trace_fn<T>(request: &http::Request<T>) -> tracing::Span {
    let span = match request.uri().path().rsplit('/').next() {
        Some("SubmitProvenTransaction") => {
            rpc_span!("block-producer.rpc", "SubmitProvenTransaction")
        },
        Some("Status") => rpc_span!("block-producer.rpc", "Status"),
        _ => {
            rpc_span!("block-producer.rpc", "Unknown")
        },
    };

    let span = add_otel_span_attributes(span, request);
    add_network_attributes(span, request)
}

fn store_rpc_trace_fn<T>(request: &http::Request<T>) -> tracing::Span {
    let method = request.uri().path().rsplit('/').next().unwrap_or("Unknown");
    let span = match method {
        "CheckNullifiers" => rpc_span!("store.rpc.rpc", "CheckNullifiers"),
        "CheckNullifiersByPrefix" => rpc_span!("store.rpc.rpc", "CheckNullifiersByPrefix"),
        "GetAccountDetails" => rpc_span!("store.rpc.rpc", "GetAccountDetails"),
        "GetAccountProofs" => rpc_span!("store.rpc.rpc", "GetAccountProofs"),
        "GetAccountStateDelta" => rpc_span!("store.rpc.rpc", "GetAccountStateDelta"),
        "GetBlockByNumber" => rpc_span!("store.rpc.rpc", "GetBlockByNumber"),
        "GetBlockHeaderByNumber" => rpc_span!("store.rpc.rpc", "GetBlockHeaderByNumber"),
        "GetNotesById" => rpc_span!("store.rpc.rpc", "GetNotesById"),
        "SyncNotes" => rpc_span!("store.rpc.rpc", "SyncNotes"),
        "SyncState" => rpc_span!("store.rpc.rpc", "SyncState"),
        "Status" => rpc_span!("store.rpc.rpc", "Status"),
        _ => rpc_span!("store.rpc.rpc", "Unknown"),
    };

    let span = add_otel_span_attributes(span, request);
    add_network_attributes(span, request)
}

fn store_block_producer_trace_fn<T>(request: &http::Request<T>) -> tracing::Span {
    let method = request.uri().path().rsplit('/').next().unwrap_or("Unknown");
    let span = match method {
        "ApplyBlock" => rpc_span!("store.block-producer.rpc", "ApplyBlock"),
        "GetBlockHeaderByNumber" => rpc_span!("store.block-producer.rpc", "GetBlockHeaderByNumber"),
        "GetBlockInputs" => rpc_span!("store.block-producer.rpc", "GetBlockInputs"),
        "GetBatchInputs" => rpc_span!("store.block-producer.rpc", "GetBatchInputs"),
        "GetTransactionInputs" => rpc_span!("store.block-producer.rpc", "GetTransactionInputs"),
        _ => rpc_span!("store.block-producer.rpc", "Unknown"),
    };

    let span = add_otel_span_attributes(span, request);
    add_network_attributes(span, request)
}

fn store_ntx_builder_trace_fn<T>(request: &http::Request<T>) -> tracing::Span {
    let method = request.uri().path().rsplit('/').next().unwrap_or("Unknown");
    let span = match method {
        "GetBlockHeaderByNumber" => rpc_span!("store.ntx-builder.rpc", "GetBlockHeaderByNumber"),
        "GetUnconsumedNetworkNotes" => {
            rpc_span!("store.ntx-builder.rpc", "GetUnconsumedNetworkNotes")
        },
        "GetCurrentBlockchainData" => {
            rpc_span!("store.ntx-builder.rpc", "GetCurrentBlockchainData")
        },
        "GetNetworkAccountDetailsByPrefix" => {
            rpc_span!("store.ntx-builder.rpc", "GetNetworkAccountDetailsByPrefix")
        },
        _ => rpc_span!("store.ntx-builder.rpc", "Unknown"),
    };

    let span = add_otel_span_attributes(span, request);
    add_network_attributes(span, request)
}

fn remote_prover_trace_fn<T>(request: &http::Request<T>) -> tracing::Span {
    let method = request.uri().path().rsplit('/').next().unwrap_or("Unknown");
    let span = match method {
        "Prove" => rpc_span!("remote-prover.rpc", "Prove"),
        "Status" => rpc_span!("remote-prover.rpc", "Status"),
        _ => rpc_span!("remote-prover.rpc", "Unknown"),
    };

    let span = add_otel_span_attributes(span, request);
    add_network_attributes(span, request)
}

fn remote_prover_proxy_trace_fn<T>(request: &http::Request<T>) -> tracing::Span {
    let method = request.uri().path().rsplit('/').next().unwrap_or("Unknown");
    let span = if method == "Status" {
        rpc_span!("remote-prover-proxy.rpc", "Status")
    } else {
        rpc_span!("remote-prover-proxy.rpc", "Unknown")
    };

    let span = add_otel_span_attributes(span, request);
    add_network_attributes(span, request)
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
