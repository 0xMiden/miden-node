use std::net::SocketAddr;

use accept::AcceptLayer;
use anyhow::Context;
use miden_node_proto::generated::rpc::api_server;
use miden_node_proto_build::rpc_api_descriptor;
use miden_node_utils::tracing::grpc::rpc_trace_fn;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic_reflection::server;
use tower_http::trace::TraceLayer;
use tracing::info;

use crate::COMPONENT;

mod accept;
mod api;

/// The RPC server component.
///
/// On startup, binds to the provided listener and starts serving the RPC API.
/// It connects lazily to the store and block producer components as needed.
/// Requests will fail if the components are not available.
pub struct Rpc {
    pub listener: TcpListener,
    pub store: SocketAddr,
    pub block_producer: Option<SocketAddr>,
}

impl Rpc {
    /// Serves the RPC API.
    ///
    /// Note: Executes in place (i.e. not spawned) and will run indefinitely until
    ///       a fatal error is encountered.
    pub async fn serve(self) -> anyhow::Result<()> {
        let api = api::RpcService::new(self.store, self.block_producer);
        let api_service = api_server::ApiServer::new(api);
        let reflection_service = server::Builder::configure()
            .register_file_descriptor_set(rpc_api_descriptor())
            .build_v1()
            .context("failed to build reflection service")?;

        info!(target: COMPONENT, endpoint=?self.listener, store=%self.store, block_producer=?self.block_producer, "Server initialized");

        tonic::transport::Server::builder()
            .accept_http1(true)
            .layer(TraceLayer::new_for_grpc().make_span_with(rpc_trace_fn))
            .layer(AcceptLayer::new()?)
            // Enables gRPC-web support.
            .add_service(tonic_web::enable(api_service))
            // Enables gRPC reflection service.
            .add_service(reflection_service)
            .serve_with_incoming(TcpListenerStream::new(self.listener))
            .await
            .context("failed to serve RPC API")
    }
}
