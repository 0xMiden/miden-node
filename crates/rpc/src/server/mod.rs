use std::net::SocketAddr;

use accept::HeaderVerificationLayer;
use anyhow::Context;
use miden_node_proto::generated::rpc::api_server;
use miden_node_proto_build::rpc_api_descriptor;
use miden_node_utils::{
    cors::cors_for_grpc_web_layer,
    tracing::grpc::{TracedComponent, traced_span_fn},
};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic_reflection::server;
use tonic_web::GrpcWebLayer;
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

        let genesis = api
            .get_genesis_header_with_retry()
            .await
            .context("Fetching genesis header from store")?;

        let api_service = api_server::ApiServer::new(api);
        let reflection_service = server::Builder::configure()
            .register_file_descriptor_set(rpc_api_descriptor())
            .build_v1()
            .context("failed to build reflection service")?;

        // This is currently required for postman to work properly because
        // it doesn't support the new version yet.
        //
        // See: <https://github.com/postmanlabs/postman-app-support/issues/13120>.
        let reflection_service_alpha = server::Builder::configure()
            .register_file_descriptor_set(rpc_api_descriptor())
            .build_v1alpha()
            .context("failed to build reflection service")?;

        info!(target: COMPONENT, endpoint=?self.listener, store=%self.store, block_producer=?self.block_producer, "Server initialized");

        tonic::transport::Server::builder()
            .accept_http1(true)
            .layer(TraceLayer::new_for_grpc().make_span_with(traced_span_fn(TracedComponent::Rpc)))
            .layer(HeaderVerificationLayer::new(genesis.commitment())?)
            .layer(cors_for_grpc_web_layer())
            // Enables gRPC-web support.
            .layer(GrpcWebLayer::new())
            .add_service(api_service)
            // Enables gRPC reflection service.
            .add_service(reflection_service)
            .add_service(reflection_service_alpha)
            .serve_with_incoming(TcpListenerStream::new(self.listener))
            .await
            .context("failed to serve RPC API")
    }
}

#[cfg(test)]
mod tests {
    use http::{
        HeaderMap, HeaderValue,
        header::{ACCEPT, CONTENT_TYPE},
    };
    use tokio::net::TcpListener;

    use crate::Rpc;

    #[tokio::test]
    async fn rpc_server_has_web_support() {
        // Start server
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let rpc_addr = listener.local_addr().unwrap();
        let rpc = Rpc {
            listener,
            store: "127.0.0.1:50051".parse().unwrap(),
            block_producer: None,
        };
        tokio::spawn(async move { rpc.serve().await.unwrap() });

        // Send a status request
        let client = reqwest::Client::new();

        let mut headers = HeaderMap::new();
        let accept_header = concat!("application/vnd.miden.", env!("CARGO_PKG_VERSION"), "+grpc");
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/grpc-web+proto"));
        headers.insert(ACCEPT, HeaderValue::from_static(accept_header));

        // An empty message with header format:
        //   - A byte indicating uncompressed (0)
        //   - A u32 indicating the data length (0)
        //
        // Originally described here:
        // https://github.com/hyperium/tonic/issues/1040#issuecomment-1191832200
        let mut message = Vec::new();
        message.push(0);
        message.extend_from_slice(&0u32.to_be_bytes());

        let response = client
            .post(format!("http://{rpc_addr}/rpc.Api/Status"))
            .headers(headers)
            .body(message)
            .send()
            .await
            .unwrap();
        let headers = response.headers();

        // CORS headers are usually set when `tonic_web` is enabled.
        //
        // This was deduced by manually checking, and isn't formally described
        // in any documentation.
        assert!(headers.get("access-control-allow-credentials").is_some());
        assert!(headers.get("access-control-expose-headers").is_some());
        assert!(headers.get("vary").is_some());
    }
}
