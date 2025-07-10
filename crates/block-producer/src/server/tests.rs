use std::time::Duration;

use miden_air::{ExecutionProof, HashFunction};
use miden_node_proto::generated::{
    block_producer::{SubmitProvenTransactionResponse, api_client as block_producer_client},
    transaction::ProvenTransaction as ProtoProvenTransaction,
};
use miden_node_store::{GenesisState, Store};
use miden_objects::{
    Digest,
    account::{AccountId, AccountIdVersion, AccountStorageMode, AccountType},
    transaction::ProvenTransactionBuilder,
};
use miden_tx::utils::Serializable;
use tokio::{net::TcpListener, runtime, task, time::sleep};
use tonic::transport::{Channel, Endpoint};
use winterfell::Proof;

use crate::{BlockProducer, SERVER_MAX_BATCHES_PER_BLOCK, SERVER_MAX_TXS_PER_BATCH};

#[tokio::test]
async fn block_producer_startup_is_robust_to_network_failures() {
    // This test starts the block producer and tests that it starts serving only after the store
    // is started.

    // get the addresses for the store and block producer
    let store_addr = {
        let store_listener =
            TcpListener::bind("127.0.0.1:0").await.expect("store should bind a port");
        store_listener.local_addr().expect("store should get a local address")
    };
    let block_producer_addr = {
        let block_producer_listener =
            TcpListener::bind("127.0.0.1:0").await.expect("failed to bind block-producer");
        block_producer_listener
            .local_addr()
            .expect("Failed to get block-producer address")
    };

    let ntx_builder_addr = {
        let ntx_builder_address = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("failed to bind the ntx builder address");
        ntx_builder_address.local_addr().expect("failed to get ntx builder address")
    };

    // start the block producer
    task::spawn(async move {
        BlockProducer {
            block_producer_address: block_producer_addr,
            store_address: store_addr,
            ntx_builder_address: Some(ntx_builder_addr),
            batch_prover_url: None,
            block_prover_url: None,
            batch_interval: Duration::from_millis(500),
            block_interval: Duration::from_millis(500),
            max_txs_per_batch: SERVER_MAX_TXS_PER_BATCH,
            max_batches_per_block: SERVER_MAX_BATCHES_PER_BLOCK,
        }
        .serve()
        .await
        .unwrap();
    });

    // test: connecting to the block producer should fail until the store is started
    let block_producer_endpoint =
        Endpoint::try_from(format!("http://{block_producer_addr}")).expect("valid url");
    let block_producer_client =
        block_producer_client::ApiClient::connect(block_producer_endpoint.clone()).await;
    assert!(block_producer_client.is_err());

    // start the store
    let data_directory = tempfile::tempdir().expect("tempdir should be created");
    let store_runtime = {
        let genesis_state = GenesisState::new(vec![], 1, 1);
        Store::bootstrap(genesis_state.clone(), data_directory.path())
            .expect("store should bootstrap");
        let dir = data_directory.path().to_path_buf();
        let rpc_listener =
            TcpListener::bind("127.0.0.1:0").await.expect("store should bind the RPC port");
        let ntx_builder_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind store ntx-builder gRPC endpoint");
        let block_producer_listener = TcpListener::bind(store_addr)
            .await
            .expect("store should bind the block-producer port");
        // in order to later kill the store, we need to spawn a new runtime and run the store on
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
        store_runtime
    };

    // we need to wait for the exponential backoff of the block producer to connect to the store
    sleep(Duration::from_secs(1)).await;

    let block_producer_client = block_producer_client::ApiClient::connect(block_producer_endpoint)
        .await
        .expect("block producer client should connect");

    // test: request against block-producer api should succeed
    let response = send_request(block_producer_client.clone(), 0).await;
    assert!(response.is_ok());

    // kill the store
    store_runtime.shutdown_background();

    // test: request against block-producer api should fail immediately
    let response = send_request(block_producer_client.clone(), 1).await;
    assert!(response.is_err());

    // test: restart the store and request should succeed
    let rpc_listener =
        TcpListener::bind("127.0.0.1:0").await.expect("store should bind the RPC port");
    let ntx_builder_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Failed to bind store ntx-builder gRPC endpoint");
    let block_producer_listener = TcpListener::bind(store_addr)
        .await
        .expect("store should bind the block-producer port");
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
    let response = send_request(block_producer_client.clone(), 2).await;
    assert!(response.is_ok());
}

/// Creates a dummy transaction and submits it to the block producer.
async fn send_request(
    mut client: block_producer_client::ApiClient<Channel>,
    i: u8,
) -> Result<tonic::Response<SubmitProvenTransactionResponse>, tonic::Status> {
    let tx = ProvenTransactionBuilder::new(
        AccountId::dummy(
            [0; 15],
            AccountIdVersion::Version0,
            AccountType::RegularAccountImmutableCode,
            AccountStorageMode::Private,
        ),
        Digest::default(),
        [i; 32].try_into().unwrap(),
        Digest::default(),
        0.into(),
        Digest::default(),
        u32::MAX.into(),
        ExecutionProof::new(Proof::new_dummy(), HashFunction::default()),
    )
    .build()
    .unwrap();
    let request = ProtoProvenTransaction { transaction: tx.to_bytes() };
    client.submit_proven_transaction(request).await
}
