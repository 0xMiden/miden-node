//! Counter increment task functionality.
//!
//! This module contains the implementation for periodically incrementing the counter
//! of the network account deployed at startup by creating and submitting network notes.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use miden_lib::AuthScheme;
use miden_lib::account::interface::AccountInterface;
use miden_lib::transaction::TransactionKernel;
use miden_lib::utils::ScriptBuilder;
use miden_node_proto::clients::{Builder, Rpc, RpcClient};
use miden_node_proto::generated::shared::BlockHeaderByNumberRequest;
use miden_node_proto::generated::transaction::ProvenTransaction;
use miden_objects::account::{
    Account,
    AccountFile,
    AccountHeader,
    AccountId,
    AuthSecretKey,
    PartialAccount,
    PublicKeyCommitment,
    StorageSlot,
};
use miden_objects::assembly::{DefaultSourceManager, Library, LibraryPath, Module, ModuleKind};
use miden_objects::asset::AssetWitness;
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
use miden_objects::transaction::{AccountInputs, InputNotes, PartialBlockchain, TransactionArgs};
use miden_objects::utils::Deserializable;
use miden_objects::{Felt, MastForest, Word, ZERO};
use miden_tx::auth::BasicAuthenticator;
use miden_tx::utils::Serializable;
use miden_tx::{
    DataStore,
    DataStoreError,
    LocalTransactionProver,
    MastForestStore,
    TransactionExecutor,
    TransactionMastStore,
};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use tokio::sync::watch;
use tokio::time::sleep;
use tracing::{error, info, instrument};

use crate::COMPONENT;
use crate::config::MonitorConfig;
use crate::status::{ServiceDetails, ServiceStatus, Status};

/// Counter increment task details.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct CounterIncrementDetails {
    /// Number of successful counter increments.
    pub success_count: u64,
    /// Number of failed counter increments.
    pub failure_count: u64,
    /// Current counter value (if available).
    pub current_value: Option<u64>,
    /// Last transaction ID (if available).
    pub last_tx_id: Option<String>,
}

async fn create_rpc_client(config: &MonitorConfig) -> Result<RpcClient> {
    Builder::new(config.rpc_url.clone())
        .with_tls()
        .context("Failed to configure TLS for RPC client")
        .unwrap()
        .with_timeout(Duration::from_secs(30))
        .without_metadata_version()
        .without_metadata_genesis()
        .connect::<Rpc>()
        .await
}

async fn get_genesis_block_header(rpc_client: &mut RpcClient) -> Result<BlockHeader> {
    // Get the latest block header
    let block_header_request = BlockHeaderByNumberRequest {
        block_num: Some(BlockNumber::GENESIS.as_u32()), // Get latest block
        include_mmr_proof: None,
    };

    let response = rpc_client
        .get_block_header_by_number(block_header_request)
        .await
        .context("Failed to get latest block header from RPC")?
        .into_inner();

    let latest_block_header = response
        .block_header
        .ok_or_else(|| anyhow::anyhow!("No block header in response"))?;

    let block_header: BlockHeader =
        latest_block_header.try_into().context("Failed to convert block header")?;

    Ok(block_header)
}

/// Wait until at least one block after `after` is available.
async fn wait_for_block_after(rpc_client: &mut RpcClient, after: BlockNumber) -> Result<()> {
    let target = after.as_u32().saturating_add(1);
    loop {
        let req = BlockHeaderByNumberRequest {
            block_num: Some(target),
            include_mmr_proof: None,
        };
        let resp = rpc_client.get_block_header_by_number(req).await?.into_inner();
        if resp.block_header.is_some() {
            return Ok(());
        }
        sleep(Duration::from_millis(500)).await;
    }
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

async fn setup_counter_increment(
    config: MonitorConfig,
    rpc_client: &mut RpcClient,
) -> Result<(
    CounterIncrementDetails,
    Account,
    Account,
    BlockHeader,
    CounterDataStore,
    NoteScript,
    SecretKey,
)> {
    let details = CounterIncrementDetails::default();
    // Load accounts from files
    let (wallet_account, secret_key) = match load_wallet_account(&config.wallet_file) {
        Ok(account) => account,
        Err(e) => {
            error!("Failed to load wallet account: {:?}", e);
            return Err(anyhow::anyhow!("Failed to load wallet account"));
        },
    };

    let counter_account = match load_counter_account(&config.counter_file) {
        Ok(account) => account,
        Err(e) => {
            error!("Failed to load counter account: {:?}", e);
            return Err(anyhow::anyhow!("Failed to load counter account"));
        },
    };

    // Get the genesis block header
    let block_header = get_genesis_block_header(rpc_client).await?;

    // Create the increment procedure script and get the library
    let (increment_script, library) = create_increment_script()?;

    // Create data store for transaction execution
    let data_store = CounterDataStore::new(
        wallet_account.clone(),
        counter_account.clone(),
        block_header.clone(),
        &library,
    );

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
/// This function periodically creates network notes that target the counter account
/// and execute the increment procedure.
///
/// # Arguments
///
/// * `config` - The monitor configuration containing file paths and intervals.
/// * `tx` - The watch channel sender for status updates.
///
/// # Returns
///
/// This function runs indefinitely, only returning on error.
#[instrument(target = COMPONENT, name = "run-counter-increment-task", skip_all, ret(level = "debug"))]
pub async fn run_counter_increment_task(
    config: MonitorConfig,
    tx: watch::Sender<ServiceStatus>,
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
    ) = setup_counter_increment(config.clone(), &mut rpc_client).await?;

    let mut rng = ChaCha20Rng::from_os_rng();

    loop {
        let mut last_error: Option<String> = None;

        // Create and submit network note
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
                wallet_account = Account::new(
                    wallet_account.id(),
                    wallet_account.vault().clone(),
                    wallet_account.storage().clone(),
                    wallet_account.code().clone(),
                    final_account.nonce(),
                    None,
                )?;

                data_store.update_wallet_account(&wallet_account);

                details.success_count += 1;
                details.last_tx_id = Some(tx_id);

                if let Err(e) = wait_for_block_after(&mut rpc_client, block_height).await {
                    error!("Failed waiting for next block: {e:?}");
                    last_error = Some(format!("wait for next block failed: {e}"));
                } else {
                    match fetch_counter_value(&mut rpc_client, counter_account.id()).await {
                        Ok(Some(nonce)) => details.current_value = Some(nonce),
                        Ok(None) => {},
                        Err(e) => {
                            error!("Failed to fetch counter value: {e:?}");
                            last_error = Some(format!("fetch counter value failed: {e}"));
                        },
                    }
                }
            },
            Err(e) => {
                error!("Failed to create and submit network note: {:?}", e);
                details.failure_count += 1;
                last_error = Some(format!("create/submit note failed: {e}"));
            },
        }

        // Update status with results
        let status = ServiceStatus {
            name: "Counter Increment".to_string(),
            status: if details.failure_count == 0 {
                Status::Healthy
            } else if details.success_count == 0 {
                Status::Unhealthy
            } else {
                Status::Healthy
            },
            last_checked: crate::monitor::tasks::current_unix_timestamp_secs(),
            error: last_error,
            details: ServiceDetails::CounterIncrement(details.clone()),
        };

        if tx.send(status).is_err() {
            error!("Failed to send counter increment status update");
            return Err(anyhow::anyhow!("Failed to send counter increment status update"));
        }

        // Wait for the next increment
        sleep(config.counter_increment_interval).await;
    }
}

/// Load wallet account from file.
fn load_wallet_account(file_path: &Path) -> Result<(Account, SecretKey)> {
    let account_file =
        AccountFile::read(file_path).context("Failed to read wallet account file")?;

    let account = account_file.account.clone();
    let auth_secret_key = account_file
        .auth_secret_keys
        .first()
        .ok_or_else(|| anyhow::anyhow!("No authentication secret key found"))?;

    let secret_key = match auth_secret_key {
        AuthSecretKey::RpoFalcon512(key) => key.clone(),
    };

    Ok((account, secret_key))
}

/// Load counter account from file.
fn load_counter_account(file_path: &Path) -> Result<Account> {
    let account_file =
        AccountFile::read(file_path).context("Failed to read counter account file")?;

    Ok(account_file.account.clone())
}

/// Create and submit a network note that targets the counter account.
#[allow(clippy::too_many_arguments)]
async fn create_and_submit_network_note(
    wallet_account: &Account,
    counter_account: &Account,
    secret_key: &SecretKey,
    rpc_client: &mut RpcClient,
    data_store: &CounterDataStore,
    block_header: &BlockHeader,
    increment_script: &NoteScript,
    rng: &mut ChaCha20Rng,
) -> Result<(String, AccountHeader, BlockNumber)> {
    // Create authenticator for transaction signing
    let pub_key = secret_key.public_key();
    let pub_key_commitment = PublicKeyCommitment::from(pub_key);
    let authenticator = BasicAuthenticator::<ChaCha20Rng>::new_with_rng(
        &[(Word::from(pub_key_commitment), AuthSecretKey::RpoFalcon512(secret_key.clone()))],
        ChaCha20Rng::from_os_rng(),
    );

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
    let request = ProvenTransaction { transaction: proven_tx.to_bytes() };

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
    let library = get_library()?;

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

/// Get the library for the counter contract.
fn get_library() -> Result<Library> {
    let assembler = TransactionKernel::assembler().with_debug_mode(true);
    let source_manager = std::sync::Arc::new(DefaultSourceManager::default());
    let script =
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/assets/counter_program.masm"));

    let library_path = LibraryPath::new("external_contract::counter_contract")
        .context("Failed to create library path")?;

    let module = Module::parser(ModuleKind::Library)
        .parse_str(library_path, script, &source_manager)
        .map_err(|e| anyhow::anyhow!("Failed to parse module: {e}"))?;

    assembler
        .clone()
        .assemble_library([module])
        .map_err(|e| anyhow::anyhow!("Failed to assemble library: {e}"))
}

/// Data store for counter increment transactions.
struct CounterDataStore {
    wallet_account: Account,
    counter_account: Account,
    block_header: BlockHeader,
    mast_store: TransactionMastStore,
}

impl CounterDataStore {
    fn new(
        wallet_account: Account,
        counter_account: Account,
        block_header: BlockHeader,
        library: &Library,
    ) -> Self {
        let mast_store = TransactionMastStore::new();
        mast_store.load_account_code(wallet_account.code());
        mast_store.load_account_code(counter_account.code());
        // Insert the counter contract library procedures
        mast_store.insert(library.mast_forest().clone());

        Self {
            wallet_account,
            counter_account,
            block_header,
            mast_store,
        }
    }

    fn update_wallet_account(&mut self, wallet_account: &Account) {
        self.wallet_account = wallet_account.clone();
        self.mast_store.load_account_code(wallet_account.code());
    }
}

impl DataStore for CounterDataStore {
    async fn get_transaction_inputs(
        &self,
        account_id: AccountId,
        mut _block_refs: std::collections::BTreeSet<BlockNumber>,
    ) -> Result<(PartialAccount, BlockHeader, PartialBlockchain), DataStoreError> {
        let account = if account_id == self.wallet_account.id() {
            &self.wallet_account
        } else {
            &self.counter_account
        };

        let partial_blockchain = PartialBlockchain::default();

        Ok((account.into(), self.block_header.clone(), partial_blockchain))
    }

    async fn get_storage_map_witness(
        &self,
        account_id: AccountId,
        map_root: Word,
        map_key: Word,
    ) -> Result<miden_objects::account::StorageMapWitness, DataStoreError> {
        let account = if account_id == self.wallet_account.id() {
            &self.wallet_account
        } else {
            &self.counter_account
        };

        let mut map_witness = None;
        for slot in account.storage().slots() {
            if let StorageSlot::Map(map) = slot {
                if map.root() == map_root {
                    map_witness = Some(map.open(&map_key));
                }
            }
        }

        if let Some(map_witness) = map_witness {
            Ok(map_witness)
        } else {
            Err(DataStoreError::Other {
                error_msg: "account storage does not contain the expected root".into(),
                source: None,
            })
        }
    }

    async fn get_foreign_account_inputs(
        &self,
        _foreign_account_id: AccountId,
        _ref_block: BlockNumber,
    ) -> Result<AccountInputs, DataStoreError> {
        unimplemented!("Not needed")
    }

    async fn get_vault_asset_witness(
        &self,
        account_id: AccountId,
        vault_root: Word,
        vault_key: Word,
    ) -> Result<AssetWitness, DataStoreError> {
        let account = if account_id == self.wallet_account.id() {
            &self.wallet_account
        } else {
            &self.counter_account
        };

        if account.vault().root() != vault_root {
            return Err(DataStoreError::Other {
                error_msg: "vault root mismatch".into(),
                source: None,
            });
        }

        AssetWitness::new(account.vault().open(vault_key).into()).map_err(|err| {
            DataStoreError::Other {
                error_msg: "failed to open vault asset tree".into(),
                source: Some(Box::new(err)),
            }
        })
    }

    async fn get_note_script(&self, script_root: Word) -> Result<NoteScript, DataStoreError> {
        Err(DataStoreError::NoteScriptNotFound(script_root))
    }
}

impl MastForestStore for CounterDataStore {
    fn get(&self, procedure_hash: &Word) -> Option<std::sync::Arc<MastForest>> {
        self.mast_store.get(procedure_hash)
    }
}
