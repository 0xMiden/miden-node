use std::{net::SocketAddr, time::Duration};

use http::{
    HeaderMap, HeaderValue,
    header::{ACCEPT, CONTENT_TYPE},
};
use miden_node_proto::{
    clients::{Builder, Rpc as RpcClientMarker, RpcClient},
    generated::{self as proto, rpc::api_client::ApiClient as ProtoClient},
};
use miden_node_store::{GenesisState, Store};
use miden_objects::{
    Felt, Word,
    account::{
        AccountDelta, AccountId, AccountIdVersion, AccountStorageDelta, AccountStorageMode,
        AccountType, AccountVaultDelta, delta::AccountUpdateDetails,
    },
    transaction::ProvenTransactionBuilder,
    utils::Serializable,
    vm::ExecutionProof,
};
use tempfile::TempDir;
use tokio::{
    net::TcpListener,
    runtime::{self, Runtime},
    task,
};
use url::Url;

use crate::Rpc;

#[tokio::test]
async fn rpc_server_accepts_requests_without_accept_header() {
    // Start the RPC.
    let (_, rpc_addr, store_addr) = start_rpc().await;
    let (store_runtime, _data_directory) = start_store(store_addr).await;

    // Override the client so that the ACCEPT header is not set.
    let mut rpc_client = {
        let endpoint = tonic::transport::Endpoint::try_from(format!("http://{rpc_addr}")).unwrap();

        ProtoClient::connect(endpoint).await.unwrap()
    };

    // Send any request to the RPC.
    let request = proto::shared::BlockHeaderByNumberRequest {
        block_num: Some(0),
        include_mmr_proof: None,
    };
    let response = rpc_client.get_block_header_by_number(request).await;

    // Assert that the server did not reject our request.
    assert!(response.is_ok());

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
    for version in ["1.9.0", "0.8.1", "0.8.0", "0.999.0", "99.0.0"] {
        // Start the RPC.
        let (_, rpc_addr, store_addr) = start_rpc().await;
        let (store_runtime, _data_directory) = start_store(store_addr).await;

        // Recreate the RPC client with an invalid version.
        let url = rpc_addr.to_string();
        // SAFETY: The rpc_addr is always valid as it is created from a `SocketAddr`.
        let url = Url::parse(format!("http://{}", &url).as_str()).unwrap();
        let mut rpc_client: RpcClient = Builder::new()
            .with_address(url.to_string())
            .with_timeout(Duration::from_secs(10))
            .with_metadata_version(version.to_string())
            .connect::<RpcClientMarker>()
            .await
            .unwrap();

        // Send any request to the RPC.
        let response = send_request(&mut rpc_client).await;

        // Assert the server does not reject our request on the basis of missing accept header.
        assert!(response.is_err());
        assert_eq!(response.as_ref().err().unwrap().code(), tonic::Code::InvalidArgument);
        assert!(response.as_ref().err().unwrap().message().contains("server does not support"),);

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
    let rpc_listener = TcpListener::bind(store_addr).await.expect("Failed to bind store");
    let ntx_builder_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Failed to bind store ntx-builder gRPC endpoint");
    let block_producer_listener =
        TcpListener::bind("127.0.0.1:0").await.expect("store should bind a port");
    task::spawn(async move {
        Store {
            rpc_listener,
            ntx_builder_listener,
            block_producer_listener,
            data_directory: data_directory.path().to_path_buf(),
        }
        .serve()
        .await
        .expect("store should start serving");
    });
    let response = send_request(&mut rpc_client).await;
    assert_eq!(response.unwrap().into_inner().block_header.unwrap().block_num, 0);
}

#[tokio::test]
async fn rpc_server_has_web_support() {
    // Start server
    let (_, rpc_addr, store_addr) = start_rpc().await;
    let (store_runtime, _data_directory) = start_store(store_addr).await;

    // Send a status request
    let client = reqwest::Client::new();

    let mut headers = HeaderMap::new();
    let accept_header = concat!("application/vnd.miden; version=", env!("CARGO_PKG_VERSION"));
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
    store_runtime.shutdown_background();
}

#[tokio::test]
async fn rpc_server_rejects_proven_transactions_with_invalid_commitment() {
    // Start the RPC.
    let (_, rpc_addr, store_addr) = start_rpc().await;
    let (store_runtime, _data_directory) = start_store(store_addr).await;

    // Override the client so that the ACCEPT header is not set.
    let mut rpc_client = {
        let endpoint = tonic::transport::Endpoint::try_from(format!("http://{rpc_addr}")).unwrap();

        ProtoClient::connect(endpoint).await.unwrap()
    };

    let account_id = AccountId::dummy(
        [0; 15],
        AccountIdVersion::Version0,
        AccountType::RegularAccountImmutableCode,
        AccountStorageMode::Public,
    );
    // Send any request to the RPC.
    let tx = ProvenTransactionBuilder::new(
        account_id,
        [8; 32].try_into().unwrap(),
        [3; 32].try_into().unwrap(),
        [22; 32].try_into().unwrap(), // delta commitment
        0.into(),
        Word::default(),
        u32::MAX.into(),
        ExecutionProof::new_dummy(),
    )
    .account_update_details(AccountUpdateDetails::Delta(
        AccountDelta::new(
            account_id,
            AccountStorageDelta::new(),
            AccountVaultDelta::default(),
            Felt::default(),
        )
        .unwrap(),
    ))
    .build()
    .unwrap();
    let request = proto::transaction::ProvenTransaction { transaction: tx.to_bytes() };

    let response = rpc_client.submit_proven_transaction(request).await;

    // Assert that the server rejected our request.
    assert!(response.is_err());

    // Assert that the error is due to the invalid account delta commitment.
    assert!(
        response
            .as_ref()
            .err()
            .unwrap()
            .message()
            .contains("Account delta commitment does not match the actual account delta"),
    );

    // Shutdown to avoid runtime drop error.
    store_runtime.shutdown_background();
}

/// Sends an arbitrary / irrelevant request to the RPC.
async fn send_request(
    rpc_client: &mut RpcClient,
) -> std::result::Result<tonic::Response<proto::shared::BlockHeaderByNumberResponse>, tonic::Status>
{
    let request = proto::shared::BlockHeaderByNumberRequest {
        block_num: Some(0),
        include_mmr_proof: None,
    };
    rpc_client.get_block_header_by_number(request).await
}

/// Binds a socket on an available port, runs the RPC server on it, and
/// returns a client to talk to the server, along with the socket address.
async fn start_rpc() -> (RpcClient, std::net::SocketAddr, std::net::SocketAddr) {
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
        // SAFETY: The store_addr is always valid as it is created from a `SocketAddr`.
        let store_url = Url::parse(&format!("http://{store_addr}")).unwrap();
        // SAFETY: The block_producer_addr is always valid as it is created from a `SocketAddr`.
        let block_producer_url = Url::parse(&format!("http://{block_producer_addr}")).unwrap();
        Rpc {
            listener: rpc_listener,
            store: store_url,
            block_producer: Some(block_producer_url),
        }
        .serve()
        .await
        .expect("Failed to start serving store");
    });
    let url = rpc_addr.to_string();
    // SAFETY: The rpc_addr is always valid as it is created from a `SocketAddr`.
    let url = Url::parse(format!("http://{}", &url).as_str()).unwrap();
    let rpc_client: RpcClient = Builder::new()
        .with_address(url.to_string())
        .with_timeout(Duration::from_secs(10))
        .connect::<RpcClientMarker>()
        .await
        .expect("Failed to build client");

    (rpc_client, rpc_addr, store_addr)
}

async fn start_store(store_addr: SocketAddr) -> (Runtime, TempDir) {
    // Start the store.
    let data_directory = tempfile::tempdir().expect("tempdir should be created");
    let genesis_state = GenesisState::new(vec![], 1, 1);
    Store::bootstrap(genesis_state.clone(), data_directory.path()).expect("store should bootstrap");
    let dir = data_directory.path().to_path_buf();
    let rpc_listener = TcpListener::bind(store_addr).await.expect("store should bind a port");
    let ntx_builder_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Failed to bind store ntx-builder gRPC endpoint");
    let block_producer_listener =
        TcpListener::bind("127.0.0.1:0").await.expect("store should bind a port");
    // In order to later kill the store, we need to spawn a new runtime and run the store on
    // it. That allows us to kill all the tasks spawned by the store when we
    // kill the runtime.
    let store_runtime =
        runtime::Builder::new_multi_thread().enable_time().enable_io().build().unwrap();
    store_runtime.spawn(async move {
        Store {
            rpc_listener,
            ntx_builder_listener,
            block_producer_listener,
            data_directory: dir,
        }
        .serve()
        .await
        .expect("store should start serving");
    });
    (store_runtime, data_directory)
}
