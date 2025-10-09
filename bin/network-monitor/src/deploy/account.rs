//! Core account deployment functionality.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use miden_lib::transaction::TransactionKernel;
use miden_lib::utils::ScriptBuilder;
use miden_node_proto::clients::{Builder, Rpc, RpcClient};
use miden_node_proto::generated::shared::BlockHeaderByNumberRequest;
use miden_node_proto::generated::transaction::ProvenTransaction;
use miden_objects::account::{
    Account,
    AccountFile,
    AccountId,
    AuthSecretKey,
    PartialAccount,
    StorageSlot,
};
use miden_objects::assembly::{DefaultSourceManager, Library, LibraryPath, Module, ModuleKind};
use miden_objects::asset::AssetWitness;
use miden_objects::block::{BlockHeader, BlockNumber};
use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;
use miden_objects::crypto::merkle::{MmrPeaks, PartialMmr};
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
use rand_chacha::ChaCha20Rng;
use tokio::sync::Mutex;
use url::Url;

/// Save wallet account to file with authentication keys.
pub fn save_wallet_account(
    account: &Account,
    secret_key: &SecretKey,
    file_path: &str,
) -> Result<()> {
    let auth_secret_key = AuthSecretKey::RpoFalcon512(secret_key.clone());
    let account_file = AccountFile::new(account.clone(), vec![auth_secret_key]);
    account_file.write(file_path)?;
    Ok(())
}

/// Save counter program account to file.
pub fn save_counter_account(account: &Account, file_path: &str) -> Result<()> {
    let account_file = AccountFile::new(account.clone(), vec![]);
    account_file.write(file_path)?;
    Ok(())
}

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
    wallet_file: &str,
    counter_file: &str,
    rpc_url: &Url,
) -> Result<()> {
    let wallet_exists = Path::new(wallet_file).exists();
    let counter_exists = Path::new(counter_file).exists();

    if wallet_exists && counter_exists {
        tracing::info!("Account files already exist, skipping account creation");
        return Ok(());
    }

    tracing::info!("Account files not found, creating new accounts");

    // Create wallet account
    let (wallet_account, secret_key) = crate::deploy::wallet::create_wallet_account()?;

    // Create counter program account
    let counter_account = crate::deploy::counter::create_counter_account(&wallet_account)?;

    // Save accounts to files
    save_wallet_account(&wallet_account, &secret_key, wallet_file)?;
    save_counter_account(&counter_account, counter_file)?;

    tracing::info!("Successfully created and saved account files");

    Box::pin(deploy_accounts(&wallet_account, &counter_account, rpc_url)).await?;

    Ok(())
}

/// Deploy accounts to the network.
///
/// This function creates both a wallet account and a counter program account,
/// then saves them to the specified files.
pub async fn deploy_accounts(
    wallet_account: &Account,
    counter_account: &Account,
    rpc_url: &Url,
) -> Result<()> {
    // Deploy accounts to the network
    let mut rpc_client: RpcClient = Builder::new(rpc_url.clone())
        .with_tls()
        .context("Failed to configure TLS for RPC client")?
        .with_timeout(Duration::from_secs(5))
        .without_metadata_version()
        .without_metadata_genesis()
        .connect::<Rpc>()
        .await
        .context("Failed to connect to RPC server")?;

    let mast_store = TransactionMastStore::new();
    mast_store.insert(wallet_account.code().mast());
    mast_store.insert(counter_account.code().mast());

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

    let data_store = Arc::new(MonitorDataStore::new(
        wallet_account.clone(),
        counter_account.clone(),
        wallet_account.seed(),
        genesis_header,
        genesis_chain_mmr,
    ));

    let executor: TransactionExecutor<'_, '_, _, BasicAuthenticator<ChaCha20Rng>> =
        TransactionExecutor::new(data_store.as_ref());

    let script_builder = ScriptBuilder::new(true)
        .with_dynamically_linked_library(&get_library(
            wallet_account.id().prefix().to_string().as_str(),
            wallet_account.id().suffix().to_string().as_str(),
        )?)
        .context("Failed to create script builder with library")?;

    let script = script_builder
        .compile_tx_script(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/increment_counter.masm"
        )))
        .context("Failed to compile transaction script")?;

    let tx_args = TransactionArgs::default().with_tx_script(script);

    let executed_tx = Box::pin(executor.execute_transaction(
        counter_account.id(),
        BlockNumber::GENESIS,
        InputNotes::default(),
        tx_args,
    ))
    .await
    .context("Failed to execute transaction")?;

    let prover = LocalTransactionProver::default();

    let proven_tx = prover.prove(executed_tx.into()).context("Failed to prove transaction")?;

    let request = ProvenTransaction { transaction: proven_tx.to_bytes() };

    rpc_client
        .submit_proven_transaction(request)
        .await
        .context("Failed to submit proven transaction to RPC")?;

    Ok(())
}

fn get_library(authorized_id_prefix: &str, authorized_id_suffix: &str) -> Result<Library> {
    let assembler = TransactionKernel::assembler().with_debug_mode(true);
    let source_manager = Arc::new(DefaultSourceManager::default());
    let script =
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/assets/counter_program.masm"));
    let script = script.replace("{AUTHORIZED_ACCOUNT_ID_PREFIX}", authorized_id_prefix);
    let script = script.replace("{AUTHORIZED_ACCOUNT_ID_SUFFIX}", authorized_id_suffix);

    let library_path = LibraryPath::new("external_contract::counter_contract")
        .context("Failed to create library path")?;

    let module = Module::parser(ModuleKind::Library)
        .parse_str(library_path, script, &source_manager)
        .map_err(|e| anyhow::anyhow!("Failed to parse module: {}", e))?;

    assembler
        .clone()
        .assemble_library([module])
        .map_err(|e| anyhow::anyhow!("Failed to assemble library: {}", e))
}

pub struct MonitorDataStore {
    wallet_account: Mutex<Account>,
    counter_account: Mutex<Account>,
    #[allow(dead_code)]
    wallet_init_seed: Option<Word>,
    block_header: BlockHeader,
    partial_block_chain: PartialBlockchain,
    mast_store: TransactionMastStore,
}

impl MonitorDataStore {
    pub fn new(
        wallet_account: Account,
        counter_account: Account,
        wallet_init_seed: Option<Word>,
        block_header: BlockHeader,
        partial_block_chain: PartialBlockchain,
    ) -> Self {
        let mast_store = TransactionMastStore::new();
        mast_store.insert(wallet_account.code().mast());
        mast_store.insert(counter_account.code().mast());

        Self {
            mast_store,
            wallet_account: Mutex::new(wallet_account),
            counter_account: Mutex::new(counter_account),
            wallet_init_seed,
            block_header,
            partial_block_chain,
        }
    }
}

impl DataStore for MonitorDataStore {
    async fn get_transaction_inputs(
        &self,
        account_id: AccountId,
        mut _block_refs: BTreeSet<BlockNumber>,
    ) -> Result<(PartialAccount, BlockHeader, PartialBlockchain), DataStoreError> {
        let account = self.wallet_account.lock().await;

        let account = if account_id == account.id() {
            account.to_owned()
        } else {
            self.counter_account.lock().await.to_owned()
        };

        Ok(((&account).into(), self.block_header.clone(), self.partial_block_chain.clone()))
    }

    async fn get_storage_map_witness(
        &self,
        account_id: AccountId,
        map_root: Word,
        map_key: Word,
    ) -> Result<miden_objects::account::StorageMapWitness, DataStoreError> {
        let account = self.wallet_account.lock().await;

        let account = if account_id == account.id() {
            account.to_owned()
        } else {
            self.counter_account.lock().await.to_owned()
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
        let account = self.wallet_account.lock().await;

        let account = if account_id == account.id() {
            account.to_owned()
        } else {
            self.counter_account.lock().await.to_owned()
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
}

impl MastForestStore for MonitorDataStore {
    fn get(&self, procedure_hash: &Word) -> Option<Arc<MastForest>> {
        self.mast_store.get(procedure_hash)
    }
}
