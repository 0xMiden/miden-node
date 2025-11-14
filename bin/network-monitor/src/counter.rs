//! Counter increment task functionality.
//!
//! This module contains the implementation for periodically incrementing the counter
//! of the network account deployed at startup by creating and submitting network notes.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use miden_lib::AuthScheme;
use miden_lib::account::interface::AccountInterface;
use miden_lib::utils::ScriptBuilder;
use miden_node_proto::clients::{Builder, Rpc, RpcClient};
use miden_node_proto::generated::shared::BlockHeaderByNumberRequest;
use miden_node_proto::generated::transaction::ProvenTransaction;
use miden_objects::account::auth::AuthSecretKey;
use miden_objects::account::{Account, AccountFile, AccountHeader, AccountId};
use miden_objects::assembly::Library;
use miden_objects::block::{BlockHeader, BlockNumber};
use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;
use miden_objects::note::{
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
use miden_objects::transaction::{InputNotes, PartialBlockchain, TransactionArgs};
use miden_objects::utils::Deserializable;
use miden_objects::{Felt, Word, ZERO};
use miden_tx::auth::BasicAuthenticator;
use miden_tx::utils::Serializable;
use miden_tx::{LocalTransactionProver, TransactionExecutor};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use tokio::sync::watch;
use tokio::time::sleep;
use tracing::{error, info, instrument};

use crate::COMPONENT;
use crate::config::MonitorConfig;
use crate::deploy::{MonitorDataStore, get_counter_library};
use crate::status::{
    CounterTrackingDetails,
    IncrementDetails,
    ServiceDetails,
    ServiceStatus,
    Status,
};

/// The smoothing factor used when calculating the exponentially weighted average latency.
///
/// Lower values mean the average reacts more slowly to new latency samples. A value of `0.9`
/// roughly mimics averaging over the last 10 samples.
const LATENCY_SMOOTHING_FACTOR: f64 = 0.9;

async fn create_rpc_client(config: &MonitorConfig) -> Result<RpcClient> {
    Builder::new(config.rpc_url.clone())
        .with_tls()
        .context("Failed to configure TLS for RPC client")
        .expect("TLS is enabled")
        .with_timeout(config.request_timeout)
        .without_metadata_version()
        .without_metadata_genesis()
        .connect::<Rpc>()
        .await
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

/// Fetch the latest nonce of the given account from RPC.
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

        let storage_slot = account.storage().slots().first().expect("storage slot is always value");
        let word = storage_slot.value();
        let value = word.as_elements().last().expect("a word is always 4 elements").as_int();

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
/// * `last_increment_timestamp` - Shared atomic timestamp for tracking when increments are sent.
///
/// # Returns
///
/// This function runs indefinitely, only returning on error.
#[instrument(target = COMPONENT, name = "run-increment-task", skip_all, ret(level = "debug"))]
pub async fn run_increment_task(
    config: MonitorConfig,
    tx: watch::Sender<ServiceStatus>,
    last_increment_timestamp: Arc<AtomicU64>,
) -> Result<()> {
    // Create RPC client
    let mut rpc_client = create_rpc_client(&config).await?;

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

    loop {
        let last_error = match create_and_submit_network_note(
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
            Ok((tx_id, final_account, _block_height)) => handle_increment_success(
                &mut wallet_account,
                &final_account,
                &mut data_store,
                &mut details,
                tx_id,
                &last_increment_timestamp,
            )?,
            Err(e) => Some(handle_increment_failure(&mut details, &e)),
        };

        let status = build_increment_status(&details, last_error);
        send_status(&tx, status)?;

        sleep(config.counter_increment_interval).await;
    }
}

/// Handle the success path for increment operations.
fn handle_increment_success(
    wallet_account: &mut Account,
    final_account: &AccountHeader,
    data_store: &mut MonitorDataStore,
    details: &mut IncrementDetails,
    tx_id: String,
    last_increment_timestamp: &Arc<AtomicU64>,
) -> Result<Option<String>> {
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

    // Record the timestamp when the increment transaction was sent
    last_increment_timestamp
        .store(crate::monitor::tasks::current_unix_timestamp_secs(), Ordering::Relaxed);

    Ok(None)
}

/// Handle the failure path when creating/submitting the network note fails.
fn handle_increment_failure(details: &mut IncrementDetails, error: &anyhow::Error) -> String {
    error!("Failed to create and submit network note: {:?}", error);
    details.failure_count += 1;
    format!("create/submit note failed: {error}")
}

/// Build a `ServiceStatus` snapshot from the current increment details and last error.
fn build_increment_status(details: &IncrementDetails, last_error: Option<String>) -> ServiceStatus {
    let status = if details.failure_count == 0 {
        Status::Healthy
    } else if details.success_count == 0 {
        Status::Unhealthy
    } else {
        Status::Healthy
    };

    ServiceStatus {
        name: "Counter Increment".to_string(),
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
/// * `last_increment_timestamp` - Shared atomic timestamp for tracking when increments are sent.
///
/// # Returns
///
/// This function runs indefinitely, only returning on error.
#[instrument(target = COMPONENT, name = "run-counter-tracking-task", skip_all, ret(level = "debug"))]
#[allow(clippy::cast_precision_loss)]
pub async fn run_counter_tracking_task(
    config: MonitorConfig,
    tx: watch::Sender<ServiceStatus>,
    last_increment_timestamp: Arc<AtomicU64>,
) -> Result<()> {
    // Create RPC client
    let mut rpc_client = create_rpc_client(&config).await?;

    // Load counter account to get the account ID
    let counter_account = match load_counter_account(&config.counter_filepath) {
        Ok(account) => account,
        Err(e) => {
            error!("Failed to load counter account: {:?}", e);
            anyhow::bail!("Failed to load counter account: {e}")
        },
    };

    let mut details = CounterTrackingDetails::default();

    loop {
        let current_time = crate::monitor::tasks::current_unix_timestamp_secs();
        let last_error = match fetch_counter_value(&mut rpc_client, counter_account.id()).await {
            Ok(Some(value)) => {
                // Only update if the counter value actually changed
                if details.current_value != Some(value) {
                    details.current_value = Some(value);
                    details.last_updated = Some(current_time);

                    // Calculate latency if we have a recent increment timestamp
                    let increment_timestamp = last_increment_timestamp.load(Ordering::Relaxed);
                    if increment_timestamp > 0 {
                        let latency_ms = (current_time - increment_timestamp) * 1000;
                        details.last_latency_ms = Some(latency_ms);

                        // Update the exponentially weighted moving average latency
                        // EWMA(t) = (α * x(t)) + ((1 - α) * EWMA(t-1))
                        let avg_latency_ms = match details.avg_latency_ms {
                            Some(avg) => {
                                (avg * LATENCY_SMOOTHING_FACTOR)
                                    + ((1.0 - LATENCY_SMOOTHING_FACTOR) * latency_ms as f64)
                            },
                            _ => latency_ms as f64,
                        };
                        details.avg_latency_ms = Some(avg_latency_ms);
                    }
                }
                None
            },
            Ok(None) => {
                // Counter value not available, but not an error
                None
            },
            Err(e) => {
                error!("Failed to fetch counter value: {:?}", e);
                Some(format!("fetch counter value failed: {e}"))
            },
        };

        let status = build_tracking_status(&details, last_error);
        send_status(&tx, status)?;

        // Fetch faster than the increment interval to ensure we don't miss any increments
        sleep(config.counter_increment_interval / 2).await;
    }
}

/// Build a `ServiceStatus` snapshot from the current tracking details and last error.
fn build_tracking_status(
    details: &CounterTrackingDetails,
    last_error: Option<String>,
) -> ServiceStatus {
    let status = if details.current_value.is_some() {
        Status::Healthy
    } else {
        Status::Unknown
    };

    ServiceStatus {
        name: "Counter Tracking".to_string(),
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
#[instrument(target = COMPONENT, name = "create-and-submit-network-note", skip_all, ret)]
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

    let account_interface = AccountInterface::new(
        wallet_account.id(),
        vec![AuthScheme::RpoFalcon512 { pub_key: secret_key.public_key().into() }],
        wallet_account.code(),
    );

    let (network_note, note_recipient) =
        create_network_note(wallet_account, counter_account, increment_script.clone(), rng)?;
    let script = account_interface.build_send_notes_script(&[network_note.into()], None, false)?;

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

    // Prove the transaction
    let prover = LocalTransactionProver::default();
    let proven_tx = prover.prove(executed_tx).context("Failed to prove transaction")?;

    // Submit the proven transaction
    let request = ProvenTransaction {
        transaction: proven_tx.to_bytes(),
        transaction_inputs: None,
    };

    let block_height: BlockNumber = rpc_client
        .submit_proven_transaction(request)
        .await
        .context("Failed to submit proven transaction to RPC")?
        .into_inner()
        .block_height
        .into();

    info!("Submitted proven transaction to RPC");

    // Use the transaction ID from the proven transaction
    let tx_id = proven_tx.id().to_hex();

    Ok((tx_id, final_account, block_height))
}

/// Create the increment procedure script.
fn create_increment_script() -> Result<(NoteScript, Library)> {
    let library = get_counter_library()?;

    let script_builder = ScriptBuilder::new(true)
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
