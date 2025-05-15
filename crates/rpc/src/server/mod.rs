use std::net::SocketAddr;

use accept::AcceptLayer;
use anyhow::Context;
use miden_node_proto::generated::rpc::api_server;
use miden_node_utils::tracing::grpc::rpc_trace_fn;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
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

        info!(target: COMPONENT, endpoint=?self.listener, store=%self.store, block_producer=?self.block_producer, "Server initialized");

        tonic::transport::Server::builder()
            .accept_http1(true)
            .layer(TraceLayer::new_for_grpc().make_span_with(rpc_trace_fn))
            .layer(AcceptLayer::new())
            // Enables gRPC-web support.
            .add_service(tonic_web::enable(api_service))
            .serve_with_incoming(TcpListenerStream::new(self.listener))
            .await
            .context("failed to serve RPC API")
    }
}

#[cfg(test)]
mod test {
    use miden_node_proto::generated::{
        requests::GetBlockHeaderByNumberRequest, responses::GetBlockHeaderByNumberResponse,
        rpc::api_client as rpc_client,
    };
    use miden_node_store::{GenesisState, Store};
    use tokio::{net::TcpListener, runtime, task};
    use tonic::{metadata::AsciiMetadataValue, service::Interceptor, transport::Endpoint};

    use crate::Rpc;

    #[tokio::test]
    async fn rpc_server_rejects_requests_without_accept_header() {
        // TODO(current pr): DRY this setup, tidy up
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

        let request = GetBlockHeaderByNumberRequest {
            block_num: Some(0),
            include_mmr_proof: None,
        };
        let response = rpc_client.get_block_header_by_number(request).await;
        assert!(response.is_err());
        assert_eq!(response.as_ref().err().unwrap().code(), tonic::Code::InvalidArgument);
        assert_eq!(response.as_ref().err().unwrap().message(), "Missing required ACCEPT header");
    }

    #[tokio::test]
    async fn rpc_server_accepts_requests_with_accept_header() {
        // TODO(current pr): DRY this setup, tidy up
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
            let channel = Endpoint::try_from(format!("http://{rpc_addr}"))
                .expect("Failed to create rpc endpoint")
                .connect()
                .await
                .unwrap();
            let version = env!("CARGO_PKG_VERSION");
            let accept_value = format!("application/vnd.miden.{version}+grpc");
            let interceptor = AcceptHeaderInterceptor::new(accept_value);
            rpc_client::ApiClient::with_interceptor(channel, interceptor)
        };

        let request = GetBlockHeaderByNumberRequest {
            block_num: Some(0),
            include_mmr_proof: None,
        };
        let response = rpc_client.get_block_header_by_number(request).await;
        assert!(response.is_err());
        assert_ne!(response.as_ref().err().unwrap().code(), tonic::Code::InvalidArgument);
        assert_ne!(response.as_ref().err().unwrap().message(), "Missing required ACCEPT header");
    }

    // TODO(current pr): move from here and tidy up
    struct AcceptHeaderInterceptor {
        value: String,
    }
    impl AcceptHeaderInterceptor {
        fn new(value: String) -> Self {
            Self { value }
        }
    }
    impl Interceptor for AcceptHeaderInterceptor {
        fn call(
            &mut self,
            request: tonic::Request<()>,
        ) -> Result<tonic::Request<()>, tonic::Status> {
            let mut request = request;
            request
                .metadata_mut()
                .insert("accept", AsciiMetadataValue::try_from(self.value.clone()).unwrap());
            Ok(request)
        }
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
            let channel = Endpoint::try_from(format!("http://{rpc_addr}"))
                .expect("Failed to create rpc endpoint")
                .connect()
                .await
                .unwrap();
            let version = env!("CARGO_PKG_VERSION");
            let accept_value = format!("application/vnd.miden.{version}+grpc");
            let interceptor = AcceptHeaderInterceptor::new(accept_value);
            rpc_client::ApiClient::with_interceptor(channel, interceptor)
        };

        // test: requests against RPC api should fail immediately
        let request = GetBlockHeaderByNumberRequest {
            block_num: Some(0),
            include_mmr_proof: None,
        };
        let response = rpc_client.get_block_header_by_number(request).await;
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
        let request = GetBlockHeaderByNumberRequest {
            block_num: Some(0),
            include_mmr_proof: None,
        };
        let response = rpc_client.get_block_header_by_number(request).await.unwrap();
        assert!(response.into_inner().block_header.is_some());

        // test: shutdown the store and should fail
        store_runtime.shutdown_background();
        let request = GetBlockHeaderByNumberRequest {
            block_num: Some(0),
            include_mmr_proof: None,
        };
        let response = rpc_client.get_block_header_by_number(request).await;
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
        let request = GetBlockHeaderByNumberRequest {
            block_num: Some(0),
            include_mmr_proof: None,
        };
        let response = rpc_client.get_block_header_by_number(request).await.unwrap();
        assert_eq!(response.into_inner().block_header.unwrap().block_num, 0);
    }
}
