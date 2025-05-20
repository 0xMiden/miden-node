use std::{net::SocketAddr, time::Duration};

use miden_node_proto::generated::{
    requests::GetBlockHeaderByNumberRequest, responses::GetBlockHeaderByNumberResponse,
    rpc::api_client::ApiClient as ProtoClient,
};
use miden_node_rpc::{ApiClient, Rpc};
use miden_node_store::{GenesisState, Store};
use tempfile::TempDir;
use tokio::{
    net::TcpListener,
    runtime::{self, Runtime},
    task,
};
use url::Url;

#[tokio::test]
async fn rpc_server_rejects_requests_without_accept_header() {
    // Start the RPC.
    let (_, rpc_addr, store_addr) = start_rpc().await;
    let (store_runtime, _data_directory) = start_store(store_addr).await;

    // Override the client so that the ACCEPT header is not set.
    let mut rpc_client = {
        let endpoint = tonic::transport::Endpoint::try_from(format!("http://{rpc_addr}")).unwrap();

        ProtoClient::connect(endpoint).await.unwrap()
    };

    // Send any request to the RPC.
    let request = GetBlockHeaderByNumberRequest {
        block_num: Some(0),
        include_mmr_proof: None,
    };
    let response = rpc_client.get_block_header_by_number(request).await;

    // Assert that the server rejected our request because of missing accept header.
    assert!(response.is_err());
    assert_eq!(response.as_ref().err().unwrap().code(), tonic::Code::InvalidArgument);
    assert_eq!(response.as_ref().err().unwrap().message(), "Missing required ACCEPT header");

    // Shutdown to avoid runtime drop error.
    store_runtime.shutdown_background();
}

#[tokio::test]
async fn rpc_server_accepts_requests_with_accept_header() {
    // Start the RPC.
    let (mut rpc_client, _, store_addr) = start_rpc().await;
    let (store_runtime, _data_directory) = start_store(store_addr).await;

    // Send any request to the RPC.
    let response = send_request(&mut rpc_client).await;

    // Assert the server does not reject our request on the basis of missing accept header.
    assert!(response.is_ok());

    // Shutdown to avoid runtime drop error.
    store_runtime.shutdown_background();
}

#[tokio::test]
async fn rpc_server_rejects_requests_with_accept_header_invalid_version() {
    // NOTE: proptest does not support async and proptest_async does not work with multiple
    // runtimes made by this fn.
    for version in ["0.1.0", "0.0.1"] {
        // Start the RPC.
        let (_, rpc_addr, store_addr) = start_rpc().await;
        let (store_runtime, _data_directory) = start_store(store_addr).await;

        // Recreate the RPC client with an invalid version.
        let url = rpc_addr.to_string();
        let url = Url::parse(format!("http://{}", &url).as_str()).unwrap();
        let mut rpc_client =
            ApiClient::connect(&url, Duration::from_secs(10), Some(version)).await.unwrap();

        // Send any request to the RPC.
        let response = send_request(&mut rpc_client).await;

        // Assert the server does not reject our request on the basis of missing accept header.
        assert!(response.is_err());
        assert_eq!(response.as_ref().err().unwrap().code(), tonic::Code::InvalidArgument);
        assert_eq!(response.as_ref().err().unwrap().message(), "Client / server version mismatch");

        // Shutdown to avoid runtime drop error.
        store_runtime.shutdown_background();
    }
}

#[tokio::test]
async fn rpc_startup_is_robust_to_network_failures() {
    // This test starts the store and RPC components and verifies that they successfully
    // connect to each other on startup and that they reconnect after the store is restarted.

    // Start the RPC.
    let (mut rpc_client, _, store_addr) = start_rpc().await;

    // Test: requests against RPC api should fail immediately
    let response = send_request(&mut rpc_client).await;
    assert!(response.is_err());

    // Start the store.
    let (store_runtime, data_directory) = start_store(store_addr).await;

    // Test: send request against RPC api and should succeed
    let response = send_request(&mut rpc_client).await;
    assert!(response.unwrap().into_inner().block_header.is_some());

    // Test: shutdown the store and should fail
    store_runtime.shutdown_background();
    let response = send_request(&mut rpc_client).await;
    assert!(response.is_err());

    // Test: restart the store and request should succeed
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
    let response = send_request(&mut rpc_client).await;
    assert_eq!(response.unwrap().into_inner().block_header.unwrap().block_num, 0);
}

/// Sends an arbitrary / irrelevant request to the RPC.
async fn send_request(
    rpc_client: &mut ApiClient,
) -> std::result::Result<tonic::Response<GetBlockHeaderByNumberResponse>, tonic::Status> {
    let request = GetBlockHeaderByNumberRequest {
        block_num: Some(0),
        include_mmr_proof: None,
    };
    rpc_client.get_block_header_by_number(request).await
}

/// Binds a socket on an available port, runs the RPC server on it, and
/// returns a client to talk to the server, along with the socket address.
async fn start_rpc() -> (ApiClient, std::net::SocketAddr, std::net::SocketAddr) {
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

    // Start the rpc component.
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
    let url = rpc_addr.to_string();
    let url = Url::parse(format!("http://{}", &url).as_str()).unwrap();
    let rpc_client = ApiClient::connect(&url, Duration::from_secs(10), None).await.unwrap();

    (rpc_client, rpc_addr, store_addr)
}

async fn start_store(store_addr: SocketAddr) -> (Runtime, TempDir) {
    // Start the store.
    let data_directory = tempfile::tempdir().expect("tempdir should be created");
    let genesis_state = GenesisState::new(vec![], 1, 1);
    Store::bootstrap(genesis_state.clone(), data_directory.path()).expect("store should bootstrap");
    let dir = data_directory.path().to_path_buf();
    let store_listener = TcpListener::bind(store_addr).await.expect("store should bind a port");
    // In order to later kill the store, we need to spawn a new runtime and run the store on
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
    (store_runtime, data_directory)
}
