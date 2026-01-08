//! Counter increment task functionality.
//!
//! This module contains the implementation for periodically incrementing the counter
//! of the network account deployed at startup by creating and submitting network notes.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use anyhow::{Context, Result};
use miden_node_proto::clients::RpcClient;
use miden_node_proto::generated::rpc::BlockHeaderByNumberRequest;
use miden_node_proto::generated::transaction::ProvenTransaction;
use miden_protocol::account::auth::AuthSecretKey;
use miden_protocol::account::{Account, AccountFile, AccountHeader, AccountId};
use miden_protocol::assembly::Library;
use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::crypto::dsa::falcon512_rpo::SecretKey;
use miden_protocol::note::{
    Note,
    NoteAssets,
    NoteExecutionHint,
    NoteInputs,
    NoteMetadata,
    NoteRecipient,
    NoteScript,
    NoteTag,
    NoteType,
};
use miden_protocol::transaction::{InputNotes, PartialBlockchain, TransactionArgs};
use miden_protocol::utils::Deserializable;
use miden_protocol::{Felt, Word, ZERO};
use miden_standards::account::interface::{AccountInterface, AccountInterfaceExt};
use miden_standards::code_builder::CodeBuilder;
use miden_tx::auth::BasicAuthenticator;
use miden_tx::utils::Serializable;
use miden_tx::{LocalTransactionProver, TransactionExecutor};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use tokio::sync::{Mutex, watch};
use tracing::{error, info, instrument, warn};

use crate::COMPONENT;
use crate::config::MonitorConfig;
use crate::deploy::counter::COUNTER_SLOT_NAME;
use crate::deploy::{MonitorDataStore, create_genesis_aware_rpc_client, get_counter_library};
use crate::status::{
    CounterTrackingDetails,
    IncrementDetails,
    PendingLatencyDetails,
    ServiceDetails,
    ServiceStatus,
    Status,
};

#[derive(Debug, Default, Clone)]
pub struct LatencyState {
    pending: Option<PendingLatencyDetails>,
    pending_started: Option<Instant>,
    last_latency_blocks: Option<u32>,
}

/// Get the genesis block header.
async fn get_genesis_block_header(rpc_client: &mut RpcClient) -> Result<BlockHeader> {
    let block_header_request = BlockHeaderByNumberRequest {
        block_num: Some(BlockNumber::GENESIS.as_u32()),
        include_mmr_proof: None,
    };

    let response = rpc_client
        .get_block_header_by_number(block_header_request)
        .await
        .context("Failed to get genesis block header from RPC")?
        .into_inner();

    let genesis_block_header = response
        .block_header
        .ok_or_else(|| anyhow::anyhow!("No block header in response"))?;

    let block_header: BlockHeader =
        genesis_block_header.try_into().context("Failed to convert block header")?;

    Ok(block_header)
}

/// Fetch the latest counter value of the given account from RPC.
async fn fetch_counter_value(
    rpc_client: &mut RpcClient,
    account_id: AccountId,
) -> Result<Option<u64>> {
    let id_bytes: [u8; 15] = account_id.into();
    let req = miden_node_proto::generated::account::AccountId { id: id_bytes.to_vec() };
    let resp = rpc_client.get_account_details(req).await?.into_inner();
    if let Some(raw) = resp.details {
        let account = Account::read_from_bytes(&raw)
            .map_err(|e| anyhow::anyhow!("failed to deserialize account details: {e}"))?;

        // Access the counter slot by name to avoid index-ordering issues
        let word = account
            .storage()
            .get_item(&COUNTER_SLOT_NAME)
            .map_err(|e| anyhow::anyhow!("failed to get counter storage slot: {e}"))?;

        let value = word.as_elements().first().expect("a word is always 4 elements").as_int();

        Ok(Some(value))
    } else {
        Ok(None)
    }
}

async fn fetch_wallet_account(
    rpc_client: &mut RpcClient,
    account_id: AccountId,
) -> Result<Option<Account>> {
    let id_bytes: [u8; 15] = account_id.into();
    let req = miden_node_proto::generated::account::AccountId { id: id_bytes.to_vec() };
    let resp = rpc_client.get_account_details(req).await;

    // If the RPC call fails, return None
    if resp.is_err() {
        return Ok(None);
    }

    let Some(account_details) = resp.expect("Previously checked for error").into_inner().details
    else {
        return Ok(None);
    };
    let account = Account::read_from_bytes(&account_details)
        .map_err(|e| anyhow::anyhow!("failed to deserialize account details: {e}"))?;

    Ok(Some(account))
}

async fn setup_increment_task(
    config: MonitorConfig,
    rpc_client: &mut RpcClient,
) -> Result<(
    IncrementDetails,
    Account,
    Account,
    BlockHeader,
    MonitorDataStore,
    NoteScript,
    SecretKey,
)> {
    let details = IncrementDetails::default();
    // Load accounts from files
    let wallet_account_file =
        AccountFile::read(config.wallet_filepath).context("Failed to read wallet account file")?;
    let wallet_account = fetch_wallet_account(rpc_client, wallet_account_file.account.id())
        .await?
        .unwrap_or(wallet_account_file.account.clone());

    let AuthSecretKey::RpoFalcon512(secret_key) = wallet_account_file
        .auth_secret_keys
        .first()
        .expect("wallet account file should have one auth secret key")
        .clone()
    else {
        anyhow::bail!("Failed to load wallet account, auth secret key not found")
    };

    let counter_account = match load_counter_account(&config.counter_filepath) {
        Ok(account) => account,
        Err(e) => {
            error!("Failed to load counter account: {:?}", e);
            anyhow::bail!("Failed to load counter account: {e}")
        },
    };

    // Get the genesis block header
    let block_header = get_genesis_block_header(rpc_client).await?;

    // Create the increment procedure script and get the library
    let (increment_script, library) = create_increment_script()?;

    // Create unified data store for transaction execution
    let mut data_store = MonitorDataStore::new(block_header.clone(), PartialBlockchain::default());
    data_store.add_account(wallet_account.clone());
    data_store.add_account(counter_account.clone());
    data_store.insert_library(&library);

    Ok((
        details,
        wallet_account,
        counter_account,
        block_header,
        data_store,
        increment_script,
        secret_key,
    ))
}

/// Run the counter increment task.
///
/// This function periodically creates network notes that target the counter account and sends
/// transactions to increment it.
///
/// # Arguments
///
/// * `config` - The monitor configuration containing file paths and intervals.
/// * `tx` - The watch channel sender for status updates.
/// * `expected_counter_value` - Shared atomic counter for tracking expected value based on
///   successful increments.
///
/// # Returns
///
/// This function runs indefinitely, only returning on error.
#[instrument(
    parent = None,
    target = COMPONENT,
    name = "network_monitor.counter.run_increment_task",
    skip_all,
    level = "info",
    ret(level = "debug"),
    err
)]
pub async fn run_increment_task(
    config: MonitorConfig,
    tx: watch::Sender<ServiceStatus>,
    expected_counter_value: Arc<AtomicU64>,
    latency_state: Arc<Mutex<LatencyState>>,
) -> Result<()> {
    // Create RPC client
    let mut rpc_client =
        create_genesis_aware_rpc_client(&config.rpc_url, config.request_timeout).await?;

    let (
        mut details,
        mut wallet_account,
        counter_account,
        block_header,
        mut data_store,
        increment_script,
        secret_key,
    ) = setup_increment_task(config.clone(), &mut rpc_client).await?;

    let mut rng = ChaCha20Rng::from_os_rng();
    let mut interval = tokio::time::interval(config.counter_increment_interval);

    loop {
        interval.tick().await;

        let mut last_error = None;

        match create_and_submit_network_note(
            &wallet_account,
            &counter_account,
            &secret_key,
            &mut rpc_client,
            &data_store,
            &block_header,
            &increment_script,
            &mut rng,
        )
        .await
        {
            Ok((tx_id, final_account, block_height)) => {
                let target_value = handle_increment_success(
                    &mut wallet_account,
                    &final_account,
                    &mut data_store,
                    &mut details,
                    tx_id,
                    &expected_counter_value,
                )?;

                {
                    let mut guard = latency_state.lock().await;
                    guard.pending = Some(PendingLatencyDetails {
                        submit_height: block_height.as_u32(),
                        target_value,
                    });
                    guard.pending_started = Some(Instant::now());
                }
            },
            Err(e) => {
                last_error = Some(handle_increment_failure(&mut details, &e));
            },
        }

        {
            let guard = latency_state.lock().await;
            details.last_latency_blocks = guard.last_latency_blocks;
        }

        let status = build_increment_status(&details, last_error);
        send_status(&tx, status)?;
    }
}

/// Handle the success path for increment operations.
///
/// Returns the next expected counter value after a successful increment.
fn handle_increment_success(
    wallet_account: &mut Account,
    final_account: &AccountHeader,
    data_store: &mut MonitorDataStore,
    details: &mut IncrementDetails,
    tx_id: String,
    expected_counter_value: &Arc<AtomicU64>,
) -> Result<u64> {
    let updated_wallet = Account::new(
        wallet_account.id(),
        wallet_account.vault().clone(),
        wallet_account.storage().clone(),
        wallet_account.code().clone(),
        final_account.nonce(),
        None,
    )?;
    *wallet_account = updated_wallet;
    data_store.update_account(wallet_account.clone());

    details.success_count += 1;
    details.last_tx_id = Some(tx_id);

    // Increment the expected counter value
    let new_expected = expected_counter_value.fetch_add(1, Ordering::Relaxed) + 1;

    Ok(new_expected)
}

/// Handle the failure path when creating/submitting the network note fails.
fn handle_increment_failure(details: &mut IncrementDetails, error: &anyhow::Error) -> String {
    error!("Failed to create and submit network note: {:?}", error);
    details.failure_count += 1;
    format!("create/submit note failed: {error}")
}

/// Build a `ServiceStatus` snapshot from the current increment details and last error.
fn build_increment_status(details: &IncrementDetails, last_error: Option<String>) -> ServiceStatus {
    let status = if last_error.is_some() {
        // If the most recent attempt failed, surface the service as unhealthy so the
        // dashboard reflects that the increment pipeline is not currently working.
        Status::Unhealthy
    } else if details.failure_count == 0 {
        Status::Healthy
    } else if details.success_count == 0 {
        Status::Unhealthy
    } else {
        Status::Healthy
    };

    ServiceStatus {
        name: "Local Transactions".to_string(),
        status,
        last_checked: crate::monitor::tasks::current_unix_timestamp_secs(),
        error: last_error,
        details: ServiceDetails::NtxIncrement(details.clone()),
    }
}

/// Send the status update, bailing on error.
fn send_status(tx: &watch::Sender<ServiceStatus>, status: ServiceStatus) -> Result<()> {
    if tx.send(status).is_err() {
        error!("Failed to send counter increment status update");
        anyhow::bail!("Failed to send counter increment status update")
    }
    Ok(())
}

/// Run the counter tracking task.
///
/// This function periodically fetches the current counter value from the network
/// and updates the tracking details.
///
/// # Arguments
///
/// * `config` - The monitor configuration containing file paths and intervals.
/// * `tx` - The watch channel sender for status updates.
/// * `expected_counter_value` - Shared atomic counter for tracking expected value based on
///   successful increments.
///
/// # Returns
///
/// This function runs indefinitely, only returning on error.
#[instrument(
    parent = None,
    target = COMPONENT,
    name = "network_monitor.counter.run_counter_tracking_task",
    skip_all,
    level = "info",
    ret(level = "debug"),
    err
)]
pub async fn run_counter_tracking_task(
    config: MonitorConfig,
    tx: watch::Sender<ServiceStatus>,
    expected_counter_value: Arc<AtomicU64>,
    latency_state: Arc<Mutex<LatencyState>>,
) -> Result<()> {
    // Create RPC client
    let mut rpc_client =
        create_genesis_aware_rpc_client(&config.rpc_url, config.request_timeout).await?;

    // Load counter account to get the account ID
    let counter_account = match load_counter_account(&config.counter_filepath) {
        Ok(account) => account,
        Err(e) => {
            error!("Failed to load counter account: {:?}", e);
            anyhow::bail!("Failed to load counter account: {e}")
        },
    };

    let mut details = CounterTrackingDetails::default();
    initialize_counter_tracking_state(
        &mut rpc_client,
        &counter_account,
        &expected_counter_value,
        &mut details,
    )
    .await;

    let mut poll_interval = tokio::time::interval(config.counter_increment_interval / 2);

    loop {
        poll_interval.tick().await;

        let last_error = poll_counter_once(
            &mut rpc_client,
            &counter_account,
            &expected_counter_value,
            &latency_state,
            &mut details,
            &config,
        )
        .await;
        let status = build_tracking_status(&details, last_error);
        send_status(&tx, status)?;
    }
}

/// Initialize tracking state by fetching the current counter value from the node.
///
/// Populates `expected_counter_value` and seeds `details` with the latest observed
/// values so the first poll iteration starts from a consistent snapshot.
async fn initialize_counter_tracking_state(
    rpc_client: &mut RpcClient,
    counter_account: &Account,
    expected_counter_value: &Arc<AtomicU64>,
    details: &mut CounterTrackingDetails,
) {
    match fetch_counter_value(rpc_client, counter_account.id()).await {
        Ok(Some(initial_value)) => {
            expected_counter_value.store(initial_value, Ordering::Relaxed);
            details.current_value = Some(initial_value);
            details.expected_value = Some(initial_value);
            details.last_updated = Some(crate::monitor::tasks::current_unix_timestamp_secs());
            info!("Initialized counter tracking with value: {}", initial_value);
        },
        Ok(None) => {
            expected_counter_value.store(0, Ordering::Relaxed);
            warn!("Counter account not found, initializing expected value to 0");
        },
        Err(e) => {
            expected_counter_value.store(0, Ordering::Relaxed);
            error!("Failed to fetch initial counter value, initializing to 0: {:?}", e);
        },
    }
}

/// Poll the counter once, updating details and latency tracking state.
///
/// Returns a human-readable error string when the poll fails or latency tracking
/// cannot complete; otherwise returns `None`.
async fn poll_counter_once(
    rpc_client: &mut RpcClient,
    counter_account: &Account,
    expected_counter_value: &Arc<AtomicU64>,
    latency_state: &Arc<Mutex<LatencyState>>,
    details: &mut CounterTrackingDetails,
    config: &MonitorConfig,
) -> Option<String> {
    let mut last_error = None;
    let current_time = crate::monitor::tasks::current_unix_timestamp_secs();

    match fetch_counter_value(rpc_client, counter_account.id()).await {
        Ok(Some(value)) => {
            details.current_value = Some(value);
            details.last_updated = Some(current_time);

            update_expected_and_pending(details, expected_counter_value, value);
            handle_latency_tracking(rpc_client, latency_state, config, value, &mut last_error)
                .await;
        },
        Ok(None) => {
            // Counter value not available, but not an error
        },
        Err(e) => {
            error!("Failed to fetch counter value: {:?}", e);
            last_error = Some(format!("fetch counter value failed: {e}"));
        },
    }

    last_error
}

/// Update expected and pending counters based on the latest observed value.
fn update_expected_and_pending(
    details: &mut CounterTrackingDetails,
    expected_counter_value: &Arc<AtomicU64>,
    observed_value: u64,
) {
    let expected = expected_counter_value.load(Ordering::Relaxed);
    details.expected_value = Some(expected);

    if expected >= observed_value {
        details.pending_increments = Some(expected - observed_value);
    } else {
        warn!(
            "Expected counter value ({}) is less than current value ({}), setting pending to 0",
            expected, observed_value
        );
        details.pending_increments = Some(0);
    }
}

/// Update latency tracking state, performing RPC as needed while minimizing lock hold time.
///
/// Populates `last_error` when latency bookkeeping fails or times out.
async fn handle_latency_tracking(
    rpc_client: &mut RpcClient,
    latency_state: &Arc<Mutex<LatencyState>>,
    config: &MonitorConfig,
    observed_value: u64,
    last_error: &mut Option<String>,
) {
    let (pending, pending_started) = {
        let guard = latency_state.lock().await;
        (guard.pending.clone(), guard.pending_started)
    };

    if let Some(pending) = pending {
        if observed_value >= pending.target_value {
            match fetch_chain_tip(rpc_client).await {
                Ok(observed_height) => {
                    let latency_blocks = observed_height.saturating_sub(pending.submit_height);
                    let mut guard = latency_state.lock().await;
                    if guard.pending.as_ref().map(|p| p.target_value) == Some(pending.target_value)
                    {
                        guard.last_latency_blocks = Some(latency_blocks);
                        guard.pending = None;
                        guard.pending_started = None;
                    }
                },
                Err(e) => {
                    *last_error = Some(format!("Failed to fetch chain tip for latency calc: {e}"));
                },
            }
        } else if let Some(started) = pending_started {
            if Instant::now().saturating_duration_since(started) >= config.counter_latency_timeout {
                warn!(
                    "Latency measurement timed out after {:?} for target value {}",
                    config.counter_latency_timeout, pending.target_value
                );
                let mut guard = latency_state.lock().await;
                if guard.pending.as_ref().map(|p| p.target_value) == Some(pending.target_value) {
                    guard.pending = None;
                    guard.pending_started = None;
                }
                *last_error = Some(format!(
                    "Timed out after {:?} waiting for counter to reach {}",
                    config.counter_latency_timeout, pending.target_value
                ));
            }
        }
    }
}

/// Build a `ServiceStatus` snapshot from the current tracking details and last error.
fn build_tracking_status(
    details: &CounterTrackingDetails,
    last_error: Option<String>,
) -> ServiceStatus {
    let status = if last_error.is_some() {
        // If the latest poll failed, surface the service as unhealthy even if we have
        // a previously cached value, so the dashboard shows that tracking is degraded.
        Status::Unhealthy
    } else if details.current_value.is_some() {
        Status::Healthy
    } else {
        Status::Unknown
    };

    ServiceStatus {
        name: "Network Transactions".to_string(),
        status,
        last_checked: crate::monitor::tasks::current_unix_timestamp_secs(),
        error: last_error,
        details: ServiceDetails::NtxTracking(details.clone()),
    }
}

/// Load counter account from file.
fn load_counter_account(file_path: &Path) -> Result<Account> {
    let account_file =
        AccountFile::read(file_path).context("Failed to read counter account file")?;

    Ok(account_file.account.clone())
}

/// Create and submit a network note that targets the counter account.
#[allow(clippy::too_many_arguments)]
#[instrument(
    parent = None,
    target = COMPONENT,
    name = "network_monitor.counter.create_and_submit_network_note",
    skip_all,
    level = "info",
    ret(level = "debug"),
    err
)]
async fn create_and_submit_network_note(
    wallet_account: &Account,
    counter_account: &Account,
    secret_key: &SecretKey,
    rpc_client: &mut RpcClient,
    data_store: &MonitorDataStore,
    block_header: &BlockHeader,
    increment_script: &NoteScript,
    rng: &mut ChaCha20Rng,
) -> Result<(String, AccountHeader, BlockNumber)> {
    // Create authenticator for transaction signing
    let authenticator = BasicAuthenticator::new(&[AuthSecretKey::RpoFalcon512(secret_key.clone())]);

    let account_interface = AccountInterface::from_account(wallet_account);

    let (network_note, note_recipient) =
        create_network_note(wallet_account, counter_account, increment_script.clone(), rng)?;
    let script = account_interface.build_send_notes_script(&[network_note.into()], None)?;

    // Create transaction executor
    let executor = TransactionExecutor::new(data_store).with_authenticator(&authenticator);

    // Execute the transaction with the network note
    let mut tx_args = TransactionArgs::default().with_tx_script(script);
    tx_args.add_output_note_recipient(Box::new(note_recipient));

    let executed_tx = Box::pin(executor.execute_transaction(
        wallet_account.id(),
        block_header.block_num(),
        InputNotes::default(),
        tx_args,
    ))
    .await
    .context("Failed to execute transaction")?;

    let final_account = executed_tx.final_account().clone();

    let tx_inputs = executed_tx.tx_inputs().to_bytes();

    // Prove the transaction
    let prover = LocalTransactionProver::default();
    let proven_tx = prover.prove(executed_tx).context("Failed to prove transaction")?;

    // Submit the proven transaction
    let request = ProvenTransaction {
        transaction: proven_tx.to_bytes(),
        transaction_inputs: Some(tx_inputs),
    };

    let block_height: BlockNumber = rpc_client
        .submit_proven_transaction(request)
        .await
        .context("Failed to submit proven transaction to RPC")?
        .into_inner()
        .block_num
        .into();

    info!("Submitted proven transaction to RPC");

    // Use the transaction ID from the proven transaction
    let tx_id = proven_tx.id().to_hex();

    Ok((tx_id, final_account, block_height))
}

/// Create the increment procedure script.
fn create_increment_script() -> Result<(NoteScript, Library)> {
    let library = get_counter_library()?;

    let script_builder = CodeBuilder::new()
        .with_dynamically_linked_library(&library)
        .context("Failed to create script builder with library")?;

    // Compile the script directly as a NoteScript
    let note_script = script_builder
        .compile_note_script(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/increment_counter.masm"
        )))
        .context("Failed to compile note script")?;

    Ok((note_script, library))
}

/// Create a network note that targets the counter account.
fn create_network_note(
    wallet_account: &Account,
    counter_account: &Account,
    script: NoteScript,
    rng: &mut ChaCha20Rng,
) -> Result<(Note, NoteRecipient)> {
    let metadata = NoteMetadata::new(
        wallet_account.id(),
        NoteType::Public,
        NoteTag::from_account_id(counter_account.id()),
        NoteExecutionHint::Always,
        ZERO,
    )?;

    let serial_num = Word::new([
        Felt::new(rng.random()),
        Felt::new(rng.random()),
        Felt::new(rng.random()),
        Felt::new(rng.random()),
    ]);

    let recipient = NoteRecipient::new(serial_num, script, NoteInputs::new(vec![])?);

    let network_note = Note::new(NoteAssets::new(vec![])?, metadata, recipient.clone());
    Ok((network_note, recipient))
}

/// Fetch the current chain tip height from RPC status.
async fn fetch_chain_tip(rpc_client: &mut RpcClient) -> Result<u32> {
    let status = rpc_client.status(()).await?.into_inner();

    if let Some(block_producer_status) = status.block_producer {
        Ok(block_producer_status.chain_tip)
    } else if let Some(store_status) = status.store {
        Ok(store_status.chain_tip)
    } else {
        anyhow::bail!("RPC status response did not include a chain tip")
    }
}
