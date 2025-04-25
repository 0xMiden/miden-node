use std::net::SocketAddr;

use anyhow::Context;
use miden_node_proto::generated::rpc::api_server;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tracing::info;

use crate::COMPONENT;

mod api;

pub struct Rpc {
    pub listener: TcpListener,
    pub store: SocketAddr,
    pub block_producer: SocketAddr,
}

impl Rpc {
    /// Serves the RPC API.
    ///
    /// Note: Executes in place (i.e. not spawned) and will run indefinitely until
    ///       a fatal error is encountered.
    pub async fn serve(self) -> anyhow::Result<()> {
        let api = api::RpcService::new(self.store, self.block_producer).unwrap();
        let api_service = api_server::ApiServer::new(api);

        info!(target: COMPONENT, "Server initialized");

        tonic::transport::Server::builder()
            .accept_http1(true)
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
                    block_producer: block_producer_addr,
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
                Store::serve(store_listener, dir).await.expect("store should start serving");
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
            Store::serve(listener, data_directory.path().to_path_buf())
                .await
                .expect("store should start serving");
        });
        let response = send_request(&mut rpc_client, 0).await.unwrap();
        assert_eq!(response.into_inner().block_header.unwrap().block_num, 0);
    }
}
