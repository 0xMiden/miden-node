//! Deploy account command implementation.
//!
//! This module contains the implementation for deploying Miden accounts to the network.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use miden_lib::transaction::TransactionKernel;
use miden_lib::utils::ScriptBuilder;
use miden_node_proto::clients::{Builder, Rpc, RpcClient};
use miden_node_proto::generated::shared::BlockHeaderByNumberRequest;
use miden_node_proto::generated::transaction::ProvenTransaction;
use miden_objects::account::{Account, AccountId, PartialAccount};
use miden_objects::assembly::{DefaultSourceManager, Library, LibraryPath, Module, ModuleKind};
use miden_objects::asset::AssetWitness;
use miden_objects::block::{BlockHeader, BlockNumber};
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

use crate::cli::DeployAccountConfig;
use crate::deploy::counter::create_counter_account;
use crate::deploy::wallet::create_wallet_account;
use crate::deploy::{save_counter_account, save_wallet_account};

/// Deploy accounts to the network.
///
/// This function creates both a wallet account and a counter program account,
/// then saves them to the specified files.
pub async fn deploy_accounts(config: DeployAccountConfig) -> Result<()> {
    // Create wallet account
    let (wallet_account, secret_key) = create_wallet_account()?;

    // Create counter program account
    let counter_account = create_counter_account(&wallet_account)?;

    // Save accounts to files
    save_wallet_account(&wallet_account, &secret_key, &config.wallet_file)?;
    save_counter_account(&counter_account, &config.counter_file)?;

    // Deploy accounts to the network
    let mut rpc_client: RpcClient = Builder::new(config.rpc_url)
        .with_tls()
        .unwrap()
        .with_timeout(Duration::from_secs(5))
        .without_metadata_version()
        .without_metadata_genesis()
        .connect::<Rpc>()
        .await
        .unwrap();

    let mast_store = TransactionMastStore::new();
    mast_store.insert(wallet_account.code().mast());
    mast_store.insert(counter_account.code().mast());

    let block_header_request = BlockHeaderByNumberRequest {
        block_num: Some(BlockNumber::GENESIS.as_u32().into()),
        include_mmr_proof: None,
    };

    let response = rpc_client.get_block_header_by_number(block_header_request).await.unwrap();

    let root_block_header = response.into_inner().block_header.unwrap();

    let genesis_header = root_block_header.try_into().unwrap();

    let genesis_chain_mmr =
        PartialBlockchain::new(PartialMmr::from_peaks(MmrPeaks::default()), Vec::new())
            .expect("Empty ChainMmr should be valid");

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
        ))
        .unwrap();
    let script = script_builder
        .compile_tx_script(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/assets/increment_counter.masm"
        )))
        .unwrap();

    let tx_args = TransactionArgs::default().with_tx_script(script);

    let executed_tx = executor
        .execute_transaction(
            counter_account.id(),
            BlockNumber::GENESIS,
            InputNotes::default(),
            tx_args,
        )
        .await
        .unwrap();

    let prover = LocalTransactionProver::default();

    let proven_tx = prover.prove(executed_tx.into()).unwrap();

    let request = ProvenTransaction { transaction: proven_tx.to_bytes() };

    rpc_client.submit_proven_transaction(request).await.unwrap();

    Ok(())
}

fn get_library(authorized_id_prefix: &str, authorized_id_suffix: &str) -> Library {
    let assembler = TransactionKernel::assembler().with_debug_mode(true);
    let source_manager = Arc::new(DefaultSourceManager::default());
    let script =
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/assets/counter_program.masm"));
    let script =
        script.replace("{AUTHORIZED_ACCOUNT_ID_PREFIX}", &authorized_id_prefix.to_string());
    let script =
        script.replace("{AUTHORIZED_ACCOUNT_ID_SUFFIX}", &authorized_id_suffix.to_string());

    let module = Module::parser(ModuleKind::Library)
        .parse_str(
            LibraryPath::new("external_contract::counter_contract").unwrap(),
            script,
            &source_manager,
        )
        .unwrap();
    assembler.clone().assemble_library([module]).unwrap()
}

pub struct MonitorDataStore {
    wallet_account: Mutex<Account>,
    counter_account: Mutex<Account>,
    _wallet_init_seed: Option<Word>,
    block_header: BlockHeader,
    partial_block_chain: PartialBlockchain,
    mast_store: TransactionMastStore,
}

impl MonitorDataStore {
    pub fn new(
        wallet_account: Account,
        counter_account: Account,
        _wallet_init_seed: Option<Word>,
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
            _wallet_init_seed,
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
        _account_id: AccountId,
        _map_root: Word,
        _map_key: Word,
    ) -> Result<miden_objects::account::StorageMapWitness, DataStoreError> {
        todo!()
    }

    async fn get_foreign_account_inputs(
        &self,
        _foreign_account_id: AccountId,
        _ref_block: BlockNumber,
    ) -> Result<AccountInputs, DataStoreError> {
        todo!()
    }

    async fn get_vault_asset_witness(
        &self,
        _account_id: AccountId,
        _vault_root: Word,
        _vault_key: Word,
    ) -> Result<AssetWitness, DataStoreError> {
        todo!()
    }
}

impl MastForestStore for MonitorDataStore {
    fn get(&self, procedure_hash: &Word) -> Option<Arc<MastForest>> {
        self.mast_store.get(procedure_hash)
    }
}
