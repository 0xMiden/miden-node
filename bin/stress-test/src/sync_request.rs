use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use miden_lib::utils::Serializable;
use miden_node_proto::generated::{
    account as account_proto, requests::SyncStateRequest, store::api_client::ApiClient,
};
use miden_node_utils::tracing::grpc::OtelInterceptor;
use miden_objects::{
    account::AccountId,
    note::{NoteExecutionMode, NoteTag},
};
use tokio::fs;
use tonic::{service::interceptor::InterceptedService, transport::Channel};

use crate::seeding::{ACCOUNTS_FILENAME, start_store};

/// Sends multiple sync requests to the store and measures the performance.
pub async fn bench_sync_request(data_directory: PathBuf, iterations: usize) {
    let start = Instant::now();

    let accounts_file = data_directory.join(ACCOUNTS_FILENAME);

    let mut store_api_client = start_store(data_directory).await;

    // read the account ids from the file. If iterations > accounts_file, repeat the account ids
    let accounts = fs::read_to_string(accounts_file).await.unwrap();
    let accounts: Vec<&str> = accounts.lines().collect();
    let mut account_ids = accounts.iter().cycle();

    // send sync requests and measure performance
    let mut timers_accumulator = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        // Take 3 account ids from the file and send a vec of them in the sync request
        let account_id_1 = (*account_ids.next().unwrap()).to_string();
        let account_id_2 = (*account_ids.next().unwrap()).to_string();
        let account_id_3 = (*account_ids.next().unwrap()).to_string();

        timers_accumulator.push(
            send_sync_request(
                &mut store_api_client,
                vec![account_id_1, account_id_2, account_id_3],
            )
            .await,
        );
    }

    let elapsed = start.elapsed();
    println!("Total sync request took: {elapsed:?}");

    let avg_time = timers_accumulator.iter().sum::<Duration>() / iterations as u32;
    println!("Average sync request took: {avg_time:?}");
}

/// Sends a single sync request to the store and returns the elapsed time.
async fn send_sync_request(
    api_client: &mut ApiClient<InterceptedService<Channel, OtelInterceptor>>,
    account_ids: Vec<String>,
) -> Duration {
    let account_ids = account_ids
        .iter()
        .map(|id| AccountId::from_hex(id).unwrap())
        .collect::<Vec<_>>();

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
    let sync_state_result = api_client.sync_state(sync_request).await;
    let elapsed = start.elapsed();

    assert!(sync_state_result.is_ok());
    elapsed
}
