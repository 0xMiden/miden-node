use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use futures::{StreamExt, stream};
use miden_lib::utils::Serializable;
use miden_node_proto::generated::{
    account as account_proto, requests::SyncStateRequest, rpc::api_client::ApiClient,
};
use miden_node_rpc::server::Rpc;
use miden_node_store::Store;
use miden_objects::{
    account::AccountId,
    note::{NoteExecutionMode, NoteTag},
};
use tokio::{fs, net::TcpListener, task};
use tonic::transport::Channel;

use crate::{metrics::compute_percentile, seeding::ACCOUNTS_FILENAME};

// LOAD TEST SYNC STATE
// ================================================================================================

/// Sends multiple sync requests to the store and measures the performance.
pub async fn test_sync_state(data_directory: PathBuf, iterations: usize, concurrency: usize) {
    let start = Instant::now();

    // load accounts from the dump file
    let accounts_file = data_directory.join(ACCOUNTS_FILENAME);
    let accounts = fs::read_to_string(accounts_file).await.unwrap();
    let mut account_ids = accounts.lines().map(|a| AccountId::from_hex(a).unwrap()).cycle();

    // start store component
    let store_addr = {
        let grpc_store = TcpListener::bind("127.0.0.1:0").await.expect("Failed to bind store");
        let store_addr = grpc_store.local_addr().expect("Failed to get store address");
        let store = Store::init(grpc_store, data_directory).await.expect("Failed to init store");
        task::spawn(async move { store.serve().await.unwrap() });
        store_addr
    };

    // create a block-producer listener without running the component, since it's not needed for the
    // sync request
    let block_producer_listener =
        TcpListener::bind("127.0.0.1:0").await.expect("Failed to bind store");
    let block_producer_addr =
        block_producer_listener.local_addr().expect("Failed to get store address");

    // start RPC component
    let rpc_listener = TcpListener::bind("127.0.0.1:0").await.expect("Failed to bind rpc");
    let rpc_url =
        format!("http://{}", rpc_listener.local_addr().expect("Failed to get rpc address"));
    let rpc = Rpc::init(rpc_listener, store_addr, block_producer_addr)
        .await
        .expect("Failed to init rpc");
    task::spawn(async move {
        rpc.serve().await.expect("Failed to serve rpc");
    });

    let rpc_client = ApiClient::connect(rpc_url).await.unwrap();

    // Create a stream of tasks to send sync_state requests.
    // Each request will have 3 account ids, 3 note tags and will be sent with block number 0.
    let tasks = stream::iter(0..iterations)
        .map(|_| {
            let mut client = rpc_client.clone();

            let account_batch: Vec<AccountId> = account_ids.by_ref().take(3).collect();

            tokio::spawn(async move {
                send_sync_request(&mut client, account_batch).await
            })
        })
        .buffer_unordered(concurrency) // ensures at most `concurrency` tasks run at the same time
        .collect::<Vec<_>>()
        .await;

    let timers_accumulator: Vec<Duration> = tasks.into_iter().map(|res| res.unwrap()).collect();

    let elapsed = start.elapsed();
    println!("Total test took: {elapsed:?}");

    let avg_time = timers_accumulator.iter().sum::<Duration>() / iterations as u32;
    println!("Average request took: {avg_time:?}");

    let p95_time = compute_percentile(timers_accumulator, 95);
    println!("P95 requests latency: {p95_time:?}");
}

/// Sends a single sync request to the store and returns the elapsed time.
async fn send_sync_request(
    api_client: &mut ApiClient<Channel>,
    account_ids: Vec<AccountId>,
) -> Duration {
    let note_tags = account_ids
        .iter()
        .map(|id| u32::from(NoteTag::from_account_id(*id, NoteExecutionMode::Local).unwrap()))
        .collect::<Vec<_>>();

    let account_ids = account_ids
        .iter()
        .map(|id| account_proto::AccountId { id: id.to_bytes() })
        .collect::<Vec<_>>();

    let sync_request = SyncStateRequest { block_num: 0, note_tags, account_ids };

    let start = Instant::now();
    api_client.sync_state(sync_request).await.unwrap();
    start.elapsed()
}
