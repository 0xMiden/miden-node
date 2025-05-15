use std::{
    net::SocketAddr,
    task::{Context as StdContext, Poll},
};

use anyhow::Context;
use futures::{FutureExt, future::BoxFuture};
use http::header::ACCEPT;
use miden_node_proto::generated::rpc::api_server;
use miden_node_utils::tracing::grpc::rpc_trace_fn;
use std::future::ready;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::body::BoxBody;
use tower::{Layer, Service};
use tower_http::trace::TraceLayer;
use tracing::{debug, info};

use crate::COMPONENT;

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

        info!(target: COMPONENT, endpoint=?self.listener, store=%self.store, block_producer=?self.block_producer, "Server initialized");

        tonic::transport::Server::builder()
            .accept_http1(true)
            .layer(TraceLayer::new_for_grpc().make_span_with(rpc_trace_fn))
            .layer(AuthLayer::new())
            // Enables gRPC-web support.
            .add_service(tonic_web::enable(api_service))
            .serve_with_incoming(TcpListenerStream::new(self.listener))
            .await
            .context("failed to serve RPC API")
    }
}

#[derive(Clone)]
pub struct AuthLayer {}

impl AuthLayer {
    pub fn new() -> Self {
        AuthLayer {}
    }
}

impl<S> Layer<S> for AuthLayer {
    type Service = AuthService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        AuthService { inner }
    }
}

#[derive(Clone)]
pub struct AuthService<S> {
    inner: S,
}

impl<S> Service<http::Request<BoxBody>> for AuthService<S>
where
    S: Service<http::Request<BoxBody>, Response = http::Response<BoxBody>> + Clone + Send + 'static,
    S::Error: Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut StdContext<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: http::Request<BoxBody>) -> Self::Future {
        // Check if ACCEPT header exists.
        if !request.headers().contains_key(ACCEPT) {
            debug!(target: COMPONENT, "Request missing ACCEPT header");

            // Create a response without calling the inner service
            let response = http::Response::builder()
                .status(http::StatusCode::BAD_REQUEST)
                .header("content-type", "application/grpc")
                .header("grpc-status", "3") // INVALID_ARGUMENT
                .header("grpc-message", "Missing required ACCEPT header")
                .body(BoxBody::default())
                .unwrap();

            // Return a future that resolves immediately to this response
            return futures::future::ready(Ok(response)).boxed();
        }

        // Otherwise proceed with normal operation
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);

        inner.call(request).boxed()
    }
}

/// Layer that checks for the existence of ACCEPT header in all incoming requests.
//#[derive(Clone)]
//struct AcceptHeaderCheckLayer;
//
//impl AcceptHeaderCheckLayer {
//    fn new() -> Self {
//        Self
//    }
//}
//
//impl<S> Layer<S> for AcceptHeaderCheckLayer {
//    type Service = AcceptHeaderCheckService<S>;
//
//    fn layer(&self, service: S) -> Self::Service {
//        AcceptHeaderCheckService { inner: service }
//    }
//}
//
///// Service that checks for the existence of ACCEPT header in all incoming requests
//#[derive(Clone)]
//struct AcceptHeaderCheckService<S> {
//    inner: S,
//}
//
////impl<T, B> tonic::codegen::Service<http::Request<B>> for ApiServer<T>
////where
////    T: Api,
////    B: Body + std::marker::Send + 'static,
////    B::Error: Into<StdError> + std::marker::Send + 'static,
//impl<S, B> Service<http::Request<B>> for AcceptHeaderCheckService<S>
//where
//    S: Service<http::Request<B>> + Clone + Send + 'static,
//    S::Future: Send + 'static,
//    B: Send + Clone + 'static,
//{
//    type Response = http::Response<BoxBody>;
//    type Error = S::Error;
//    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;
//
//    fn poll_ready(&mut self, cx: &mut StdContext<'_>) -> Poll<Result<(), Self::Error>> {
//        self.inner.poll_ready(cx)
//    }
//
//    fn call(&mut self, request: http::Request<B>) -> Self::Future {
//        let clone = self.inner.clone();
//        let mut inner = std::mem::replace(&mut self.inner, clone);
//
//        Box::pin(async move {
//            // Check if ACCEPT header exists
//            if !request.headers().contains_key(ACCEPT) {
//                debug!(target: COMPONENT, "Request missing ACCEPT header");
//                let response = http::Response::builder()
//                    .status(http::StatusCode::BAD_REQUEST)
//                    .header("content-type", "application/grpc")
//                    .header("grpc-status", "3") // INVALID_ARGUMENT
//                    .header("grpc-message", "Missing required ACCEPT header")
//                    .body(BoxBody::default())
//                    .unwrap();
//                return Ok(response);
//            }
//
//            // Forward the request to the inner service
//            inner.call(request).await
//        })
//    }
//}

#[cfg(test)]
mod test {
    use miden_node_proto::generated::{
        requests::GetBlockHeaderByNumberRequest, responses::GetBlockHeaderByNumberResponse,
        rpc::api_client as rpc_client,
    };
    use miden_node_store::{GenesisState, Store};
    use tokio::{net::TcpListener, runtime, task};
    use tonic::transport::Endpoint;

    use crate::Rpc;

    /// Sends a `get_block_header_by_number` request to the RPC server with block number 0.
    async fn send_request(
        rpc_client: &mut rpc_client::ApiClient<tonic::transport::Channel>,
        block_num: u32,
    ) -> Result<tonic::Response<GetBlockHeaderByNumberResponse>, tonic::Status> {
        let request = GetBlockHeaderByNumberRequest {
            block_num: Some(block_num),
            include_mmr_proof: None,
        };

        rpc_client.get_block_header_by_number(request).await
    }

    #[tokio::test]
    async fn rpc_startup_is_robust_to_network_failures() {
        // This test starts the store and RPC components and verifies that they successfully
        // connect to each other on startup and that they reconnect after the store is restarted.

        // get the addresses for the store and block producer
        let store_addr = {
            let store_listener =
                TcpListener::bind("127.0.0.1:0").await.expect("store should bind a port");
            store_listener.local_addr().expect("store should get a local address")
        };
        let block_producer_addr = {
            let block_producer_listener =
                TcpListener::bind("127.0.0.1:0").await.expect("Failed to bind block-producer");
            block_producer_listener
                .local_addr()
                .expect("Failed to get block-producer address")
        };
        // start the rpc component
        let mut rpc_client = {
            let rpc_listener = TcpListener::bind("127.0.0.1:0").await.expect("Failed to bind rpc");
            let rpc_addr = rpc_listener.local_addr().expect("Failed to get rpc address");
            task::spawn(async move {
                Rpc {
                    listener: rpc_listener,
                    store: store_addr,
                    block_producer: Some(block_producer_addr),
                }
                .serve()
                .await
                .expect("Failed to start serving store");
            });
            let rpc_endpoint = Endpoint::try_from(format!("http://{rpc_addr}"))
                .expect("Failed to create rpc endpoint");

            rpc_client::ApiClient::connect(rpc_endpoint)
                .await
                .expect("Failed to create rpc client")
        };

        // test: requests against RPC api should fail immediately
        let response = send_request(&mut rpc_client, 0).await;
        assert!(response.is_err());

        // start the store
        let data_directory = tempfile::tempdir().expect("tempdir should be created");
        let store_runtime = {
            let genesis_state = GenesisState::new(vec![], 1, 1);
            Store::bootstrap(genesis_state.clone(), data_directory.path())
                .expect("store should bootstrap");
            let dir = data_directory.path().to_path_buf();
            let store_listener =
                TcpListener::bind(store_addr).await.expect("store should bind a port");
            // in order to later kill the store, we need to spawn a new runtime and run the store on
            // it. That allows us to kill all the tasks spawned by the store when we
            // kill the runtime.
            let store_runtime =
                runtime::Builder::new_multi_thread().enable_time().enable_io().build().unwrap();
            store_runtime.spawn(async move {
                Store {
                    listener: store_listener,
                    data_directory: dir,
                }
                .serve()
                .await
                .expect("store should start serving");
            });
            store_runtime
        };

        // test: send request against RPC api and should succeed
        let response = send_request(&mut rpc_client, 0).await.unwrap();
        assert!(response.into_inner().block_header.is_some());

        // test: shutdown the store and should fail
        store_runtime.shutdown_background();
        let response = send_request(&mut rpc_client, 0).await;
        assert!(response.is_err());

        // test: restart the store and request should succeed
        let listener = TcpListener::bind(store_addr).await.expect("Failed to bind store");
        task::spawn(async move {
            Store {
                listener,
                data_directory: data_directory.path().to_path_buf(),
            }
            .serve()
            .await
            .expect("store should start serving");
        });
        let response = send_request(&mut rpc_client, 0).await.unwrap();
        assert_eq!(response.into_inner().block_header.unwrap().block_num, 0);
    }
}
