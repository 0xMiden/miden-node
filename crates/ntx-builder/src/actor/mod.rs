pub mod account_state;
mod inflight_note;
mod note_state;

use std::sync::Arc;

use account_state::{NetworkAccountState, TransactionCandidate};
use futures::FutureExt;
use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_proto::domain::mempool::MempoolEvent;
use miden_node_utils::ErrorReport;
use miden_objects::account::Account;
use miden_objects::block::BlockNumber;
use miden_objects::transaction::TransactionId;
use miden_remote_prover_client::remote_prover::tx_prover::RemoteTransactionProver;
use tokio::sync::{AcquireError, RwLock, Semaphore, mpsc};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::block_producer::BlockProducerClient;
use crate::builder::ChainState;
use crate::store::StoreClient;
use crate::transaction::NtxError;

// ACTOR SHUTDOWN REASON
// ================================================================================================

/// The reason an actor has shut down.
pub enum ActorShutdownReason {
    AccountReverted(NetworkAccountPrefix),
    EventChannelClosed,
    SemaphoreFailed(AcquireError),
    Cancelled(NetworkAccountPrefix),
}

// ACCOUNT ACTOR CONFIG
// ================================================================================================

/// Configuration that is required by all account actors.
#[derive(Debug, Clone)]
pub struct AccountActorConfig {
    /// Client for interacting with the store in order to load account state.
    pub store: StoreClient,
    /// Address of the block producer gRPC server.
    pub block_producer_url: Url,
    /// Address of the remote prover. If `None`, transactions will be proven locally, which is
    // undesirable due to the performance impact.
    pub tx_prover_url: Option<Url>,
    /// The latest chain state that account actors can rely on.
    pub chain_state: Arc<RwLock<ChainState>>,
}

// ACCOUNT ORIGIN
// ================================================================================================

/// The origin of the account which the actor will use to initialize the account state.
#[derive(Debug)]
pub enum AccountOrigin {
    /// Accounts that have been created by a transaction.
    Transaction(Box<Account>),
    /// Accounts that already exist in the store.
    Store(NetworkAccountPrefix),
}

impl AccountOrigin {
    /// Returns an [`AccountOrigin::Transaction`] if the account is a network account.
    pub fn transaction(account: &Account) -> Option<Self> {
        if account.is_network() {
            Some(AccountOrigin::Transaction(account.clone().into()))
        } else {
            None
        }
    }

    /// Returns an [`AccountOrigin::Store`].
    pub fn store(prefix: NetworkAccountPrefix) -> Self {
        AccountOrigin::Store(prefix)
    }

    /// Returns the [`NetworkAccountPrefix`] of the account.
    pub fn prefix(&self) -> NetworkAccountPrefix {
        match self {
            AccountOrigin::Transaction(account) => NetworkAccountPrefix::try_from(account.id())
                .expect("actor accounts are always network accounts"),
            AccountOrigin::Store(prefix) => *prefix,
        }
    }
}

// ACTOR MODE
// ================================================================================================

/// The mode of operation that the account actor is currently performing.
#[derive(Default, Debug)]
enum ActorMode {
    #[default]
    NoViableNotes,
    NotesAvailable,
    TransactionInflight(TransactionId),
}

// ACCOUNT ACTOR
// ================================================================================================

/// Independant actor that manages state and processes transactions for a single network account.
pub struct AccountActor {
    origin: AccountOrigin,
    store: StoreClient,
    mode: ActorMode,
    event_rx: mpsc::Receiver<MempoolEvent>,
    cancel_token: CancellationToken,
    block_producer: BlockProducerClient,
    prover: Option<RemoteTransactionProver>,
    chain_state: Arc<RwLock<ChainState>>,
}

impl AccountActor {
    /// Constructs a new account actor and corresponding messaging channel with the given
    /// configuration.
    pub fn new(
        origin: AccountOrigin,
        config: &AccountActorConfig,
        event_rx: mpsc::Receiver<MempoolEvent>,
        cancel_token: CancellationToken,
    ) -> Self {
        let block_producer = BlockProducerClient::new(config.block_producer_url.clone());
        let prover = config.tx_prover_url.clone().map(RemoteTransactionProver::new);
        Self {
            origin,
            store: config.store.clone(),
            mode: ActorMode::default(),
            event_rx,
            cancel_token,
            block_producer,
            prover,
            chain_state: config.chain_state.clone(),
        }
    }

    /// Runs the account actor, processing events and managing state until a reason to shutdown is
    /// encountered.
    pub async fn run(mut self, semaphore: Arc<Semaphore>) -> ActorShutdownReason {
        // Load the account state from the store and set up the account actor state.
        let account = {
            match self.origin {
                AccountOrigin::Store(account_prefix) => self
                    .store
                    .get_network_account(account_prefix)
                    .await
                    .expect("actor should be able to load account")
                    .expect("actor account should exist"),
                AccountOrigin::Transaction(ref account) => *(account.clone()),
            }
        };
        let block_num = self.chain_state.read().await.chain_tip_header.block_num();
        let mut state =
            NetworkAccountState::load(account, self.origin.prefix(), &self.store, block_num)
                .await
                .expect("actor should be able to load account state");

        loop {
            // Enable or disable transaction execution based on actor mode.
            let tx_permit_acquisition = match self.mode {
                // Disable transaction execution.
                ActorMode::NoViableNotes | ActorMode::TransactionInflight(_) => {
                    std::future::pending().boxed()
                },
                // Enable transaction execution.
                ActorMode::NotesAvailable => semaphore.acquire().boxed(),
            };
            tokio::select! {
                _ = self.cancel_token.cancelled() => {
                    return ActorShutdownReason::Cancelled(self.origin.prefix());
                }
                // Handle mempool events.
                event = self.event_rx.recv() => {
                    let Some(event) = event else {
                         return ActorShutdownReason::EventChannelClosed;
                    };
                    // Re-enable transaction execution if the transaction being waited on has been
                    // added to the mempool.
                    if let ActorMode::TransactionInflight(ref awaited_id) = self.mode {
                        if let MempoolEvent::TransactionAdded { id, .. } = &event {
                            if id == awaited_id {
                                self.mode = ActorMode::NotesAvailable;
                            }
                        }
                    } else {
                        self.mode = ActorMode::NotesAvailable;
                    }
                    // Update state.
                    if let Some(shutdown_reason) = state.mempool_update(event).await {
                        return shutdown_reason;
                    }
                },
                // Execute transactions.
                permit = tx_permit_acquisition => {
                    match permit {
                        Ok(_permit) => {
                            // Read the chain state.
                            let chain_state = self.chain_state.read().await.clone();
                            // Find a candidate transaction and execute it.
                            if let Some(tx_candidate) = state.select_candidate(crate::MAX_NOTES_PER_TX, chain_state) {
                                self.execute_transactions(&mut state, tx_candidate).await;
                            } else {
                                // No transactions to execute, wait for events.
                                self.mode = ActorMode::NoViableNotes;
                            }
                        }
                        Err(err) => {
                            return ActorShutdownReason::SemaphoreFailed(err);
                        }
                    }
                }
            }
        }
    }

    /// Execute a transaction candidate and mark notes as failed as required.
    ///
    /// Updates the state of the actor based on the execution result.
    #[tracing::instrument(name = "ntx.actor.execute_transactions", skip(self, state, tx_candidate))]
    async fn execute_transactions(
        &mut self,
        state: &mut NetworkAccountState,
        tx_candidate: TransactionCandidate,
    ) {
        let block_num = tx_candidate.chain_tip_header.block_num();

        // Execute the selected transaction.
        let context = crate::transaction::NtxContext {
            block_producer: self.block_producer.clone(),
            prover: self.prover.clone(),
        };

        let execution_result = context.execute_transaction(tx_candidate).await;
        match execution_result {
            // Execution completed without failed notes.
            Ok((tx_id, failed)) if failed.is_empty() => {
                self.mode = ActorMode::TransactionInflight(tx_id);
            },
            // Execution completed with some failed notes.
            Ok((tx_id, failed)) => {
                let notes = failed.into_iter().map(|note| note.note).collect::<Vec<_>>();
                state.notes_failed(notes.as_slice(), block_num);
                self.mode = ActorMode::TransactionInflight(tx_id);
            },
            // Transaction execution failed.
            Err(err) => {
                tracing::error!(err = err.as_report(), "network transaction failed");
                match err {
                    NtxError::AllNotesFailed(failed) => {
                        let notes = failed.into_iter().map(|note| note.note).collect::<Vec<_>>();
                        state.notes_failed(notes.as_slice(), block_num);
                        self.mode = ActorMode::NoViableNotes;
                    },
                    NtxError::InputNotes(_)
                    | NtxError::NoteFilter(_)
                    | NtxError::Execution(_)
                    | NtxError::Proving(_)
                    | NtxError::Submission(_)
                    | NtxError::Panic(_) => {
                        self.mode = ActorMode::NoViableNotes;
                    },
                }
            },
        }
    }
}

// HELPERS
// ================================================================================================

/// Checks if the backoff block period has passed.
///
/// The number of blocks passed since the last attempt must be greater than or equal to
/// e^(0.25 * `attempt_count`) rounded to the nearest integer.
///
/// This evaluates to the following:
/// - After 1 attempt, the backoff period is 1 block.
/// - After 3 attempts, the backoff period is 2 blocks.
/// - After 10 attempts, the backoff period is 12 blocks.
/// - After 20 attempts, the backoff period is 148 blocks.
/// - etc...
#[allow(clippy::cast_precision_loss, clippy::cast_sign_loss)]
fn has_backoff_passed(
    chain_tip: BlockNumber,
    last_attempt: Option<BlockNumber>,
    attempts: usize,
) -> bool {
    if attempts == 0 {
        return true;
    }
    // Compute the number of blocks passed since the last attempt.
    let blocks_passed = last_attempt
        .and_then(|last| chain_tip.checked_sub(last.as_u32()))
        .unwrap_or_default();

    // Compute the exponential backoff threshold: Δ = e^(0.25 * n).
    let backoff_threshold = (0.25 * attempts as f64).exp().round() as usize;

    // Check if the backoff period has passed.
    blocks_passed.as_usize() > backoff_threshold
}
