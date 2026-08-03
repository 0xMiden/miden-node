//! Account deployment module.
//!
//! This module contains functionality for deploying Miden accounts to the network.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use backon::{ExponentialBuilder, Retryable};
use miden_node_proto::clients::{Builder, RpcClient};
use miden_node_proto::domain::encryption::{
    TransactionInputsSealer,
    TrustedTransactionEncryptionState,
    verify_transaction_encryption_key,
};
use miden_node_proto::generated::rpc::BlockHeaderByNumberRequest;
use miden_node_proto::generated::transaction::ProvenTransaction as ProtoProvenTransaction;
use miden_node_utils::retry;
use miden_node_utils::spawn::spawn_blocking_in_current_span;
use miden_node_utils::tracing::miden_instrument;
use miden_protocol::Word;
use miden_protocol::account::{Account, AccountId, PartialAccount, StorageMapKey};
use miden_protocol::asset::{AssetId, AssetWitness};
use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::PublicKey as ValidatorPublicKey;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use miden_protocol::crypto::merkle::mmr::{MmrPeaks, PartialMmr};
use miden_protocol::note::{NoteScript, NoteScriptRoot};
use miden_protocol::transaction::{
    AccountInputs,
    ExecutedTransaction,
    InputNotes,
    PartialBlockchain,
    ProvenTransaction,
    TransactionArgs,
    TransactionInputs,
};
use miden_protocol::utils::serde::Serializable;
use miden_tx::auth::BasicAuthenticator;
use miden_tx::{
    DataStore,
    DataStoreError,
    LoadedMastForest,
    LocalTransactionProver,
    MastForestStore,
    TransactionExecutor,
    TransactionMastStore,
};
use tokio::sync::Mutex;
use url::Url;

use crate::deploy::counter::create_counter_account;
use crate::deploy::wallet::create_wallet_account;
use crate::{COMPONENT, LOG_TARGET};

pub mod counter;
pub mod wallet;

/// Monitor accounts and signing key created as one deployment unit.
pub struct DeployedMonitorAccounts {
    pub wallet: Account,
    pub secret_key: SecretKey,
    pub counter: Account,
}

/// RPC client and verified transaction-input sealer shared by monitor submission workflows.
#[derive(Clone)]
pub struct TransactionSubmissionClient {
    rpc_client: RpcClient,
    genesis_commitment: Word,
    trusted_validator_signing_keys: Arc<[ValidatorPublicKey]>,
    sealer: Arc<Mutex<Option<TransactionInputsSealer>>>,
}

impl TransactionSubmissionClient {
    /// Connects to RPC and pins the validator key trusted for encryption-key attestations.
    pub async fn connect(
        rpc_url: &Url,
        timeout: Duration,
        trusted_validator_signing_key: ValidatorPublicKey,
    ) -> Result<Self> {
        let (rpc_client, genesis_commitment) =
            create_genesis_aware_rpc_client(rpc_url, timeout).await?;
        let client = Self {
            rpc_client,
            genesis_commitment,
            trusted_validator_signing_keys: Arc::from([trusted_validator_signing_key]),
            sealer: Arc::new(Mutex::new(None)),
        };
        client.sealer().await?;
        Ok(client)
    }

    /// Returns a clone of the underlying RPC client for read operations.
    pub fn rpc_client(&self) -> RpcClient {
        self.rpc_client.clone()
    }

    /// Returns the cached verified sealer, fetching and checking the attested key on first use.
    async fn sealer(&self) -> Result<TransactionInputsSealer> {
        if let Some(sealer) = self.sealer.lock().await.clone() {
            return Ok(sealer);
        }

        let key = self
            .rpc_client
            .clone()
            .get_transaction_encryption_key(())
            .await
            .context("Failed to fetch the transaction encryption key")?
            .into_inner();
        let verified = verify_transaction_encryption_key(
            key,
            TrustedTransactionEncryptionState::new(
                self.genesis_commitment,
                &self.trusted_validator_signing_keys,
            ),
        )
        .context("Untrusted transaction encryption key")?;
        let sealer = TransactionInputsSealer::new(verified);

        let mut cached = self.sealer.lock().await;
        if let Some(sealer) = cached.clone() {
            return Ok(sealer);
        }
        *cached = Some(sealer.clone());
        Ok(sealer)
    }

    /// Seals and submits one proven transaction, retrying once with a fresh key when needed.
    pub async fn submit(
        &self,
        proven_tx: &ProvenTransaction,
        transaction_inputs: &[u8],
    ) -> Result<BlockNumber> {
        let transaction = proven_tx.to_bytes();
        let tx_id = proven_tx.id();
        let stale_key = AtomicBool::new(false);

        let result = (|| {
            let transaction = transaction.clone();
            async {
                if stale_key.swap(false, Ordering::Relaxed) {
                    *self.sealer.lock().await = None;
                }

                let sealed = self
                    .sealer()
                    .await?
                    .seal(tx_id, transaction_inputs)
                    .context("Failed to seal the transaction inputs")?;
                self.rpc_client
                    .clone()
                    .submit_proven_tx(ProtoProvenTransaction {
                        transaction,
                        sealed_transaction_inputs: Some(sealed),
                    })
                    .await
                    .context("Failed to submit proven transaction to RPC")
            }
        })
        .retry(retry::constant(Duration::ZERO, Some(1)))
        .when(|err: &anyhow::Error| {
            err.downcast_ref::<tonic::Status>()
                .is_some_and(|status| status.code() == tonic::Code::FailedPrecondition)
        })
        .notify(|status: &anyhow::Error, _| {
            stale_key.store(true, Ordering::Relaxed);
            tracing::warn!(
                target: COMPONENT,
                %tx_id,
                err = %status,
                "Transaction inputs rejected as stale, refreshing the encryption key and retrying",
            );
        })
        .await;

        Ok(result?.into_inner().block_num.into())
    }
}

/// Backoff schedule applied to the genesis-discovery RPC handshake.
///
/// At startup the monitor may come up before the node's RPC endpoint is accepting connections, so
/// the eager `connect()` (and the follow-up `get_block_header_by_number` request) is retried with
/// exponential backoff instead of failing on the first refused connection. The schedule is bounded
/// so a single handshake attempt returns within a few minutes; callers that must survive a
/// genuinely unreachable endpoint (e.g. the NTX bootstrap in `monitor::tasks`) wrap it in their
/// own unbounded retry loop.
const GENESIS_DISCOVERY_BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const GENESIS_DISCOVERY_BACKOFF_MAX: Duration = Duration::from_secs(30);
const GENESIS_DISCOVERY_MAX_RETRIES: usize = 10;

/// Builds the [`ExponentialBuilder`] used to back off retries on transient genesis-discovery
/// failures.
fn genesis_discovery_backoff() -> ExponentialBuilder {
    ExponentialBuilder::default()
        .with_min_delay(GENESIS_DISCOVERY_BACKOFF_INITIAL)
        .with_max_delay(GENESIS_DISCOVERY_BACKOFF_MAX)
        .with_factor(2.0)
        .with_max_times(GENESIS_DISCOVERY_MAX_RETRIES)
        .with_jitter()
}

/// Create an RPC client configured with the correct genesis metadata in the `Accept` header so that
/// write RPCs such as `SubmitProvenTx` are accepted by the node.
///
/// The full handshake (genesis discovery plus the genesis-aware reconnect) is retried with
/// [`genesis_discovery_backoff`] so a node that is still starting up does not abort the monitor.
pub async fn create_genesis_aware_rpc_client(
    rpc_url: &Url,
    timeout: Duration,
) -> Result<(RpcClient, Word)> {
    (|| async {
        // First, create a temporary client without genesis metadata to discover the genesis block
        // header and its commitment.
        let mut rpc: RpcClient = Builder::new(rpc_url.clone())
            .with_tls()
            .context("Failed to configure TLS for RPC client")?
            .with_timeout(timeout)
            .without_metadata_version()
            .without_metadata_genesis()
            .without_otel_context_injection()
            .connect()
            .await
            .context("Failed to create RPC client for genesis discovery")?;

        let block_header_request = BlockHeaderByNumberRequest {
            block_num: Some(BlockNumber::GENESIS.as_u32()),
            include_mmr_proof: None,
        };

        let response = rpc
            .get_block_header_by_number(block_header_request)
            .await
            .context("Failed to get genesis block header from RPC")?
            .into_inner();

        let genesis_block_header = response
            .block_header
            .ok_or_else(|| anyhow::anyhow!("No block header in response"))?;

        let genesis_header: BlockHeader =
            genesis_block_header.try_into().context("Failed to convert block header")?;
        let genesis_commitment = genesis_header.commitment();
        // Rebuild the client, this time including the required genesis metadata so that write RPCs
        // like SubmitProvenTx are accepted by the node.
        let rpc_client = Builder::new(rpc_url.clone())
            .with_tls()
            .context("Failed to configure TLS for RPC client")?
            .with_timeout(timeout)
            .without_metadata_version()
            .with_metadata_genesis(genesis_commitment)
            .without_otel_context_injection()
            .connect()
            .await
            .context("Failed to connect to RPC server with genesis metadata")?;

        Ok((rpc_client, genesis_commitment))
    })
    .retry(genesis_discovery_backoff())
    .notify(|err: &anyhow::Error, sleep: Duration| {
        tracing::warn!(
            target: COMPONENT,
            err = ?err,
            sleep_ms = sleep.as_millis() as u64,
            "RPC genesis discovery failed; retrying after backoff",
        );
    })
    .await
}

/// Create a fresh wallet + counter pair in memory and deploy the counter to the network.
///
/// Used both at startup and by the increment task when accounts are fundamentally outdated
/// (e.g., after a network reset) and re-syncing from the RPC is not sufficient. The accounts
/// are never persisted to disk; the monitor re-creates them on every restart.
pub async fn create_and_deploy_accounts(
    submission_client: &TransactionSubmissionClient,
    prover: &LocalTransactionProver,
) -> Result<DeployedMonitorAccounts> {
    tracing::info!(target: LOG_TARGET, "Creating fresh monitor accounts");

    let (wallet_account, secret_key) = create_wallet_account()?;
    let counter_account = create_counter_account(wallet_account.id())?;

    deploy_counter_account(&counter_account, submission_client, prover).await?;
    tracing::info!(target: LOG_TARGET, "Successfully created and deployed accounts");

    Ok(DeployedMonitorAccounts {
        wallet: wallet_account,
        secret_key,
        counter: counter_account,
    })
}

/// Execute the counter account's genesis (creation) transaction in-memory.
///
/// Fetches the genesis block header from RPC, builds a [`MonitorDataStore`] over it, and executes
/// the creation transaction. Does not prove or submit.
async fn execute_counter_genesis_tx(
    counter_account: &Account,
    rpc_client: &mut RpcClient,
) -> Result<ExecutedTransaction> {
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

    let genesis_header: BlockHeader =
        root_block_header.try_into().context("Failed to convert block header")?;

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

    Ok(executed_tx)
}

/// Build a valid set of transaction inputs for a throwaway counter genesis transaction.
///
/// Used as the static payload for the remote-prover probe: it produces a real, self-consistent
/// transaction the remote prover can re-execute and prove, without depending on the network
/// transaction service or any pre-existing on-chain account. The only network access is a single
/// RPC read for the genesis block header; nothing is proven or submitted here.
pub async fn build_probe_transaction_inputs(rpc_url: &Url) -> Result<TransactionInputs> {
    let (wallet_account, _secret_key) = create_wallet_account()?;
    let counter_account = create_counter_account(wallet_account.id())?;

    let (mut rpc_client, _) =
        create_genesis_aware_rpc_client(rpc_url, Duration::from_secs(10)).await?;
    let executed_tx = execute_counter_genesis_tx(&counter_account, &mut rpc_client).await?;

    Ok(executed_tx.tx_inputs().clone())
}

/// Deploy a counter account to the network by submitting its genesis transaction via RPC.
#[miden_instrument(
    target = COMPONENT,
    name = "deploy-counter-account",
    ret(level = "debug"),
)]
pub async fn deploy_counter_account(
    counter_account: &Account,
    submission_client: &TransactionSubmissionClient,
    prover: &LocalTransactionProver,
) -> Result<()> {
    let mut rpc_client = submission_client.rpc_client();

    let executed_tx = execute_counter_genesis_tx(counter_account, &mut rpc_client).await?;

    let transaction_inputs = executed_tx.tx_inputs().to_bytes();

    let prover = prover.clone();
    let proven_tx = spawn_blocking_in_current_span(move || prover.prove(executed_tx))
        .await
        .context("prover task panicked")?
        .context("Failed to prove transaction")?;

    submission_client.submit(&proven_tx, &transaction_inputs).await?;

    Ok(())
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
        let account = self.get_account(account_id)?;
        let partial_account = PartialAccount::from(account);

        Ok((partial_account, self.block_header.clone(), self.partial_block_chain.clone()))
    }

    async fn get_storage_map_witness(
        &self,
        _account_id: AccountId,
        _map_root: Word,
        _map_key: StorageMapKey,
    ) -> Result<miden_protocol::account::StorageMapWitness, DataStoreError> {
        unimplemented!("Not needed")
    }

    async fn get_foreign_account_inputs(
        &self,
        _foreign_account_id: AccountId,
        _ref_block: BlockNumber,
    ) -> Result<AccountInputs, DataStoreError> {
        unimplemented!("Not needed")
    }

    async fn get_vault_asset_witnesses(
        &self,
        account_id: AccountId,
        vault_root: Word,
        vault_keys: BTreeSet<AssetId>,
    ) -> Result<Vec<AssetWitness>, DataStoreError> {
        let account = self.get_account(account_id)?;

        if account.vault().root() != vault_root {
            return Err(DataStoreError::Other {
                error_msg: "vault root mismatch".into(),
                source: None,
            });
        }

        Result::<Vec<_>, _>::from_iter(vault_keys.into_iter().map(|vault_key| {
            AssetWitness::new(account.vault().open(vault_key).into(), [vault_key]).map_err(|err| {
                DataStoreError::Other {
                    error_msg: "failed to open vault asset tree".into(),
                    source: Some(Box::new(err)),
                }
            })
        }))
    }

    async fn get_note_script(
        &self,
        _script_root: NoteScriptRoot,
    ) -> Result<Option<NoteScript>, DataStoreError> {
        Ok(None)
    }
}

impl MastForestStore for MonitorDataStore {
    fn get(&self, procedure_hash: &Word) -> Option<LoadedMastForest> {
        self.mast_store.get(procedure_hash)
    }
}
