use miden_node_proto::generated::{
    requests::GetBlockHeaderByNumberRequest, responses::GetBlockHeaderByNumberResponse,
    rpc::api_client as rpc_client,
};
use miden_node_store::GenesisState;
use tokio::{net::TcpListener, runtime, task};
use tonic::transport::Endpoint;

#[tokio::test]
async fn test_components_startup() {
    // This test starts the store and RPC components and verifies that they
    // successfully connect to each other on startup and that they reconnect after the store is
    // restarted.

    // bootstrap and start the store
    let data_directory = tempfile::tempdir().expect("tempdir should be created");
    let genesis_state = GenesisState::new(vec![], 1, 1);
    miden_node_store::bootstrap(genesis_state.clone(), data_directory.path())
        .expect("store should bootstrap");

    let store_listener = TcpListener::bind("127.0.0.1:0").await.expect("store should bind a port");
    let store_addr = store_listener.local_addr().expect("store should get a local address");
    let dir = data_directory.path().to_path_buf();

    // in order to kill the store, we need to spawn a new runtime and run the store on it. That
    // allows us to kill all the tasks spawned by the store when we kill the runtime.
    let store_runtime =
        runtime::Builder::new_multi_thread().enable_time().enable_io().build().unwrap();
    store_runtime.spawn(async move {
        miden_node_store::serve(store_listener, dir)
            .await
            .expect("store should start serving");
    });

    // start the rpc component
    let rpc_listener = TcpListener::bind("127.0.0.1:0").await.expect("Failed to bind rpc");
    let rpc_addr = rpc_listener.local_addr().expect("Failed to get rpc address");
    let block_producer_listener =
        TcpListener::bind("127.0.0.1:0").await.expect("Failed to bind block-producer");
    let block_producer_addr = block_producer_listener
        .local_addr()
        .expect("Failed to get block-producer address");
    task::spawn(async move {
        miden_node_rpc::serve(rpc_listener, store_addr, block_producer_addr)
            .await
            .expect("Failed to start serving store");
    });

    // test send request against rpc api and request should succeed
    let endpoint =
        Endpoint::try_from(format!("http://{rpc_addr}")).expect("Failed to create endpoint");
    let mut rpc_client = rpc_client::ApiClient::connect(endpoint)
        .await
        .expect("Failed to create rpc client");

    let response = send_request(&mut rpc_client, 1).await.unwrap();
    assert!(response.into_inner().block_header.is_none());

    // test shutdown the store and request should fail
    store_runtime.shutdown_background();
    let response = send_request(&mut rpc_client, 0).await;
    assert!(response.is_err());

    // test restart the store and request should succeed
    let listener = TcpListener::bind(store_addr).await.expect("Failed to bind store");
    task::spawn(async move {
        miden_node_store::serve(listener, data_directory.path().to_path_buf())
            .await
            .expect("store should start serving");
    });
    let response = send_request(&mut rpc_client, 0).await.unwrap();
    assert_eq!(response.into_inner().block_header.unwrap().block_num, 0);
}

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
