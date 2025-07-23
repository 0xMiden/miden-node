use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use futures::{StreamExt, stream};
use miden_node_proto::generated::{self as proto, rpc_store::rpc_client::RpcClient};
use miden_node_utils::tracing::grpc::OtelInterceptor;
use miden_objects::{
    account::AccountId,
    note::{NoteDetails, NoteTag},
    utils::{Deserializable, Serializable},
};
use tokio::fs;
use tonic::{service::interceptor::InterceptedService, transport::Channel};

use crate::{
    seeding::{ACCOUNTS_FILENAME, start_store},
    store::metrics::print_summary,
};

mod metrics;

// SYNC STATE
// ================================================================================================

/// Sends multiple `sync_state` requests to the store and prints the performance.
///
/// Arguments:
/// - `data_directory`: directory that contains the database dump file and the accounts ids dump
///   file.
/// - `iterations`: number of requests to send.
/// - `concurrency`: number of requests to send in parallel.
pub async fn bench_sync_state(data_directory: PathBuf, iterations: usize, concurrency: usize) {
    // load accounts from the dump file
    let accounts_file = data_directory.join(ACCOUNTS_FILENAME);
    let accounts = fs::read_to_string(&accounts_file)
        .await
        .expect(format!("missing file {}", accounts_file.display()).as_str());
    let mut account_ids = accounts.lines().map(|a| AccountId::from_hex(a).unwrap()).cycle();

    let (store_client, _) = start_store(data_directory).await;

    // each request will have 3 account ids, 3 note tags and will be sent with block number 0
    let request = |_| {
        let mut client = store_client.clone();
        let account_batch: Vec<AccountId> = account_ids.by_ref().take(3).collect();
        tokio::spawn(async move { sync_state(&mut client, account_batch, 0).await })
    };

    // create a stream of tasks to send sync_notes requests
    let (timers_accumulator, responses) = stream::iter(0..iterations)
        .map(request)
        .buffer_unordered(concurrency)
        .map(|res| res.unwrap())
        .collect::<(Vec<_>, Vec<_>)>()
        .await;

    print_summary(timers_accumulator);

    #[allow(clippy::cast_precision_loss)]
    let average_notes_per_response =
        responses.iter().map(|r| r.notes.len()).sum::<usize>() as f64 / responses.len() as f64;
    println!("Average notes per response: {average_notes_per_response}");
}

/// Sends a single `sync_state` request to the store and returns a tuple with:
/// - the elapsed time.
/// - the response.
pub async fn sync_state(
    api_client: &mut RpcClient<InterceptedService<Channel, OtelInterceptor>>,
    account_ids: Vec<AccountId>,
    block_num: u32,
) -> (Duration, proto::rpc_store::SyncStateResponse) {
    let note_tags = account_ids
        .iter()
        .map(|id| u32::from(NoteTag::from_account_id(*id)))
        .collect::<Vec<_>>();

    let account_ids = account_ids
        .iter()
        .map(|id| proto::account::AccountId { id: id.to_bytes() })
        .collect::<Vec<_>>();

    let sync_request = proto::rpc_store::SyncStateRequest { block_num, note_tags, account_ids };

    let start = Instant::now();
    let response = api_client.sync_state(sync_request).await.unwrap();
    (start.elapsed(), response.into_inner())
}

// SYNC NOTES
// ================================================================================================

/// Sends multiple `sync_notes` requests to the store and prints the performance.
///
/// Arguments:
/// - `data_directory`: directory that contains the database dump file and the accounts ids dump
///   file.
/// - `iterations`: number of requests to send.
/// - `concurrency`: number of requests to send in parallel.
pub async fn bench_sync_notes(data_directory: PathBuf, iterations: usize, concurrency: usize) {
    // load accounts from the dump file
    let accounts_file = data_directory.join(ACCOUNTS_FILENAME);
    let accounts = fs::read_to_string(&accounts_file)
        .await
        .expect(format!("missing file {}", accounts_file.display()).as_str());
    let mut account_ids = accounts.lines().map(|a| AccountId::from_hex(a).unwrap()).cycle();

    let (store_client, _) = start_store(data_directory).await;

    // each request will have 3 note tags and will be sent with block number 0.
    let request = |_| {
        let mut client = store_client.clone();
        let account_batch: Vec<AccountId> = account_ids.by_ref().take(3).collect();
        tokio::spawn(async move { sync_notes(&mut client, account_batch).await })
    };

    // create a stream of tasks to send the requests
    let timers_accumulator = stream::iter(0..iterations)
        .map(request)
        .buffer_unordered(concurrency)
        .map(|res| res.unwrap())
        .collect::<Vec<_>>()
        .await;

    print_summary(timers_accumulator);
}

/// Sends a single `sync_notes` request to the store and returns the elapsed time.
/// The note tags are generated from the account ids, so the request will contain a note tag for
/// each account id, with a block number of 0.
pub async fn sync_notes(
    api_client: &mut RpcClient<InterceptedService<Channel, OtelInterceptor>>,
    account_ids: Vec<AccountId>,
) -> Duration {
    let note_tags = account_ids
        .iter()
        .map(|id| u32::from(NoteTag::from_account_id(*id)))
        .collect::<Vec<_>>();
    let sync_request = proto::rpc_store::SyncNotesRequest { block_num: 0, note_tags };

    let start = Instant::now();
    api_client.sync_notes(sync_request).await.unwrap();
    start.elapsed()
}

// CHECK NULLIFIERS BY PREFIX
// ================================================================================================

/// Sends multiple `check_nullifiers_by_prefix` requests to the store and prints the performance.
///
/// Arguments:
/// - `data_directory`: directory that contains the database dump file and the accounts ids dump
///   file.
/// - `iterations`: number of requests to send.
/// - `concurrency`: number of requests to send in parallel.
/// - `prefixes_per_request`: number of prefixes to send in each request.
pub async fn bench_check_nullifiers_by_prefix(
    data_directory: PathBuf,
    iterations: usize,
    concurrency: usize,
    prefixes_per_request: usize,
) {
    let (mut store_client, _) = start_store(data_directory.clone()).await;

    let accounts_file = data_directory.join(ACCOUNTS_FILENAME);
    let accounts = fs::read_to_string(&accounts_file)
        .await
        .expect(format!("missing file {}", accounts_file.display()).as_str());
    let account_ids: Vec<AccountId> =
        accounts.lines().map(|a| AccountId::from_hex(a).unwrap()).collect();

    // get all nullifier prefixes from the store
    let mut nullifier_prefixes: Vec<u32> = vec![];
    let mut current_block_num = 0;
    loop {
        // get the accounts notes
        let (_, response) =
            sync_state(&mut store_client, account_ids.clone(), current_block_num).await;
        let note_ids = response
            .notes
            .iter()
            .map(|n| n.note_id.unwrap())
            .collect::<Vec<proto::note::NoteId>>();

        // get the notes nullifiers.
        let notes = store_client
            .get_notes_by_id(proto::note::NoteIdList { ids: note_ids })
            .await
            .unwrap()
            .into_inner()
            .notes;

        nullifier_prefixes.extend(
            notes
                .iter()
                .filter_map(|n| {
                    // private notes are filtered out because `n.details` is None
                    let details_bytes = n.note.as_ref()?.details.as_ref()?;
                    let details = NoteDetails::read_from_bytes(details_bytes).unwrap();
                    Some(u32::from(details.nullifier().prefix()))
                })
                .collect::<Vec<u32>>(),
        );

        current_block_num = response.block_header.unwrap().block_num;
        if response.chain_tip == current_block_num {
            break;
        }
    }
    let mut nullifiers = nullifier_prefixes.into_iter().cycle();

    // each request will have `prefixes_per_request` prefixes and block number 0
    let request = |_| {
        let mut client = store_client.clone();

        let nullifiers_batch: Vec<u32> = nullifiers.by_ref().take(prefixes_per_request).collect();

        tokio::spawn(async move { check_nullifiers_by_prefix(&mut client, nullifiers_batch).await })
    };

    // create a stream of tasks to send the requests
    let (timers_accumulator, responses) = stream::iter(0..iterations)
        .map(request)
        .buffer_unordered(concurrency)
        .map(|res| res.unwrap())
        .collect::<(Vec<_>, Vec<_>)>()
        .await;

    print_summary(timers_accumulator);

    #[allow(clippy::cast_precision_loss)]
    let average_nullifiers_per_response =
        responses.iter().map(|r| r.nullifiers.len()).sum::<usize>() as f64 / responses.len() as f64;
    println!("Average nullifiers per response: {average_nullifiers_per_response}");
}

/// Sends a single `check_nullifiers_by_prefix` request to the store and returns:
/// - the elapsed time.
/// - the response.
async fn check_nullifiers_by_prefix(
    api_client: &mut RpcClient<InterceptedService<Channel, OtelInterceptor>>,
    nullifiers_prefixes: Vec<u32>,
) -> (Duration, proto::rpc_store::CheckNullifiersByPrefixResponse) {
    let sync_request = proto::rpc_store::CheckNullifiersByPrefixRequest {
        nullifiers: nullifiers_prefixes,
        prefix_len: 16,
        block_num: 0,
    };

    let start = Instant::now();
    let response = api_client.check_nullifiers_by_prefix(sync_request).await.unwrap();
    (start.elapsed(), response.into_inner())
}
