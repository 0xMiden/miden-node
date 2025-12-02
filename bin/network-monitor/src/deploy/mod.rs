//! Account deployment module.
//!
//! This module contains functionality for deploying Miden accounts to the network.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use miden_lib::transaction::TransactionKernel;
use miden_node_proto::clients::{Builder, RpcClient};
use miden_node_proto::generated::rpc::BlockHeaderByNumberRequest;
use miden_node_proto::generated::transaction::ProvenTransaction;
use miden_objects::account::{Account, AccountId, PartialAccount, PartialStorage};
use miden_objects::assembly::{DefaultSourceManager, Library, LibraryPath, Module, ModuleKind};
use miden_objects::asset::{AssetVaultKey, AssetWitness, PartialVault};
use miden_objects::block::{BlockHeader, BlockNumber};
use miden_objects::crypto::merkle::{MmrPeaks, PartialMmr};
use miden_objects::note::NoteScript;
use miden_objects::transaction::{AccountInputs, InputNotes, PartialBlockchain, TransactionArgs};
use miden_objects::{MastForest, Word};
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
use tracing::instrument;
use url::Url;

use crate::COMPONENT;
use crate::deploy::counter::{create_counter_account, save_counter_account};
use crate::deploy::wallet::{create_wallet_account, save_wallet_account};

pub mod counter;
pub mod wallet;

/// Ensure accounts exist, creating them if they don't.
///
/// This function checks if the wallet and counter account files exist.
/// If they don't exist, it creates new accounts and saves them to the specified files.
/// If they do exist, it does nothing.
///
/// # Arguments
///
/// * `wallet_file` - Path to the wallet account file.
/// * `counter_file` - Path to the counter program account file.
///
/// # Returns
///
/// `Ok(())` if the accounts exist or were successfully created, or an error if creation fails.
pub async fn ensure_accounts_exist(
    wallet_filepath: &Path,
    counter_filepath: &Path,
    rpc_url: &Url,
) -> Result<()> {
    let wallet_exists = wallet_filepath.exists();
    let counter_exists = counter_filepath.exists();

    if wallet_exists && counter_exists {
        tracing::info!("Account files already exist, skipping account creation");
        return Ok(());
    }

    tracing::info!("Account files not found, creating new accounts");

    // Create wallet account
    let (wallet_account, secret_key) = create_wallet_account()?;

    // Create counter program account
    let counter_account = create_counter_account(wallet_account.id())?;

    deploy_counter_account(&counter_account, rpc_url).await?;
    tracing::info!("Successfully created and deployed accounts");

    // Save accounts to files
    save_wallet_account(&wallet_account, &secret_key, wallet_filepath)?;
    save_counter_account(&counter_account, counter_filepath)
}

/// Deploy counter account to the network.
///
/// This function creates a counter program account,
/// then saves it to the specified file.
#[instrument(target = COMPONENT, name = "deploy-counter-account", skip_all, ret(level = "debug"))]
pub async fn deploy_counter_account(counter_account: &Account, rpc_url: &Url) -> Result<()> {
    // Deploy counter account to the network
    let mut rpc_client: RpcClient = Builder::new(rpc_url.clone())
        .with_tls()
        .context("Failed to configure TLS for RPC client")?
        .with_timeout(Duration::from_secs(5))
        .without_metadata_version()
        .without_metadata_genesis()
        .without_otel_context_injection()
        .connect()
        .await
        .context("Failed to connect to RPC server")?;

    let block_header_request = BlockHeaderByNumberRequest {
        block_num: Some(BlockNumber::GENESIS.as_u32()),
        include_mmr_proof: None,
    };

    let response = rpc_client
        .get_block_header_by_number(block_header_request)
        .await
        .context("Failed to get block header from RPC")?;

    let root_block_header = response
        .into_inner()
        .block_header
        .ok_or_else(|| anyhow::anyhow!("No block header in response"))?;

    let genesis_header = root_block_header.try_into().context("Failed to convert block header")?;

    let genesis_chain_mmr =
        PartialBlockchain::new(PartialMmr::from_peaks(MmrPeaks::default()), Vec::new())
            .context("Failed to create empty ChainMmr")?;

    let mut data_store = MonitorDataStore::new(genesis_header, genesis_chain_mmr);
    data_store.add_account(counter_account.clone());

    let executor: TransactionExecutor<'_, '_, _, BasicAuthenticator> =
        TransactionExecutor::new(&data_store);

    let tx_args = TransactionArgs::default();

    let executed_tx = executor
        .execute_transaction(
            counter_account.id(),
            BlockNumber::GENESIS,
            InputNotes::default(),
            tx_args,
        )
        .await
        .context("Failed to execute transaction")?;

    let prover = LocalTransactionProver::default();

    let proven_tx = prover.prove(executed_tx).context("Failed to prove transaction")?;

    let request = ProvenTransaction {
        transaction: proven_tx.to_bytes(),
        transaction_inputs: None,
    };

    rpc_client
        .submit_proven_transaction(request)
        .await
        .context("Failed to submit proven transaction to RPC")?;

    Ok(())
}

pub(crate) fn get_counter_library() -> Result<Library> {
    let assembler = TransactionKernel::assembler().with_debug_mode(true);
    let source_manager = Arc::new(DefaultSourceManager::default());
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

// MONITOR DATA STORE
// ================================================================================================

/// A [`DataStore`] implementation for the network monitor.
pub struct MonitorDataStore {
    accounts: HashMap<AccountId, Account>,
    block_header: BlockHeader,
    partial_block_chain: PartialBlockchain,
    mast_store: TransactionMastStore,
}

impl MonitorDataStore {
    pub fn new(block_header: BlockHeader, partial_block_chain: PartialBlockchain) -> Self {
        Self {
            accounts: HashMap::new(),
            block_header,
            partial_block_chain,
            mast_store: TransactionMastStore::new(),
        }
    }

    /// Add or replace an account in the store and load its code into the MAST store.
    pub fn add_account(&mut self, account: Account) {
        self.mast_store.load_account_code(account.code());
        self.accounts.insert(account.id(), account);
    }

    /// Update an account after a transaction (loads latest code too).
    pub fn update_account(&mut self, account: Account) {
        self.add_account(account);
    }

    /// Insert external library procedures used by transactions.
    pub fn insert_library(&mut self, library: &Library) {
        self.mast_store.insert(library.mast_forest().clone());
    }

    /// Returns a reference to the account or a standardized "unknown account" error.
    fn get_account(&self, account_id: AccountId) -> Result<&Account, DataStoreError> {
        self.accounts.get(&account_id).ok_or_else(|| DataStoreError::Other {
            error_msg: "unknown account".into(),
            source: None,
        })
    }
}

impl DataStore for MonitorDataStore {
    async fn get_transaction_inputs(
        &self,
        account_id: AccountId,
        mut _block_refs: BTreeSet<BlockNumber>,
    ) -> Result<(PartialAccount, BlockHeader, PartialBlockchain), DataStoreError> {
        let account = self.get_account(account_id)?.clone();
        let partial_storage = PartialStorage::new_full(account.storage().clone());
        let assert_vault = PartialVault::new_full(account.vault().clone());
        let partial_account = PartialAccount::new(
            account_id,
            account.nonce(),
            account.code().clone(),
            partial_storage,
            assert_vault,
            account.seed(),
        )
        .expect("Partial account be valid");

        Ok((partial_account, self.block_header.clone(), self.partial_block_chain.clone()))
    }

    async fn get_storage_map_witness(
        &self,
        _account_id: AccountId,
        _map_root: Word,
        _map_key: Word,
    ) -> Result<miden_objects::account::StorageMapWitness, DataStoreError> {
        unimplemented!("Not needed")
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
        vault_key: AssetVaultKey,
    ) -> Result<AssetWitness, DataStoreError> {
        let account = self.get_account(account_id)?;

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

    async fn get_note_script(
        &self,
        _script_root: Word,
    ) -> Result<Option<NoteScript>, DataStoreError> {
        Ok(None)
    }
}

impl MastForestStore for MonitorDataStore {
    fn get(&self, procedure_hash: &Word) -> Option<Arc<MastForest>> {
        self.mast_store.get(procedure_hash)
    }
}
