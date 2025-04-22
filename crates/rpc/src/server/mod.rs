use std::net::SocketAddr;

use anyhow::Context;
use miden_node_proto::generated::rpc::api_server;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tracing::info;

use crate::COMPONENT;

mod api;

/// Serves the RPC API.
///
/// Note: this blocks until the server dies.
pub async fn serve(
    listener: TcpListener,
    store: SocketAddr,
    block_producer: SocketAddr,
) -> anyhow::Result<()> {
    info!(target: COMPONENT, endpoint=?listener, %store, %block_producer, "Initializing server");

    let api = api::RpcService::new(store, block_producer)?;
    let api_service = api_server::ApiServer::new(api);

    info!(target: COMPONENT, "Server initialized");

    tonic::transport::Server::builder()
        .accept_http1(true)
        .add_service(tonic_web::enable(api_service))  // tonic_web::enable is needed to support grpc-web calls
        .serve_with_incoming(TcpListenerStream::new(listener))
        .await
        .context("failed to serve RPC API")
}
