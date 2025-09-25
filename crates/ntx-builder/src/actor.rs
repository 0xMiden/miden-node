use std::sync::Arc;

use futures::FutureExt;
use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_proto::domain::mempool::MempoolEvent;
use miden_node_utils::ErrorReport;
use miden_objects::account::Account;
use miden_objects::transaction::TransactionId;
use miden_remote_prover_client::remote_prover::tx_prover::RemoteTransactionProver;
use tokio::sync::{AcquireError, RwLock, Semaphore, mpsc};
use url::Url;

use crate::block_producer::BlockProducerClient;
use crate::builder::ChainState;
use crate::state::{State, TransactionCandidate};
use crate::store::StoreClient;
use crate::transaction::NtxError;

pub enum ActorShutdownReason {
    AccountReverted(NetworkAccountPrefix),
    EventChannelClosed,
    SemaphoreFailed(AcquireError),
}

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

/// The mode of operation that the account actor is currently performing.
#[derive(Default, Debug)]
enum ActorMode {
    #[default]
    AwaitingEvents,
    ExecutingTransactions,
    AwaitingCommit(TransactionId),
}

/// Account actor that manages state and processes transactions for a single network account.
pub struct AccountActor {
    origin: AccountOrigin,
    store: StoreClient,
    state: ActorMode,
    event_rx: mpsc::UnboundedReceiver<MempoolEvent>,
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
    ) -> (Self, mpsc::UnboundedSender<MempoolEvent>) {
        let block_producer = BlockProducerClient::new(config.block_producer_url.clone());
        let prover = config.tx_prover_url.clone().map(RemoteTransactionProver::new);
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let actor = Self {
            origin,
            store: config.store.clone(),
            state: ActorMode::default(),
            event_rx,
            block_producer,
            prover,
            chain_state: config.chain_state.clone(),
        };
        (actor, event_tx)
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
        let mut state = State::load(account, self.origin.prefix(), &self.store, block_num)
            .await
            .expect("actor should be able to load account state");

        loop {
            // Enable or disable transaction execution based on actor mode.
            let tx_permit_acquisition = match self.state {
                // Disable transaction execution.
                ActorMode::AwaitingEvents | ActorMode::AwaitingCommit(_) => {
                    std::future::pending().boxed()
                },
                // Enable transaction execution.
                ActorMode::ExecutingTransactions => semaphore.acquire().boxed(),
            };
            tokio::select! {
                // Handle mempool events.
                event = self.event_rx.recv() => {
                    let Some(event) = event else {
                         return ActorShutdownReason::EventChannelClosed;
                    };
                    // Re-enable transaction execution if the transaction being waited on has been
                    // added to the mempool.
                    if let ActorMode::AwaitingCommit(ref awaited_id) = self.state {
                        if let MempoolEvent::TransactionAdded { id, .. } = &event {
                            if id == awaited_id {
                                self.state = ActorMode::ExecutingTransactions;
                            }
                        }
                    } else {
                        self.state = ActorMode::ExecutingTransactions;
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
                                self.state = ActorMode::AwaitingEvents;
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
        state: &mut State,
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
                self.state = ActorMode::AwaitingCommit(tx_id);
            },
            // Execution completed with some failed notes.
            Ok((tx_id, failed)) => {
                let notes = failed.into_iter().map(|note| note.note).collect::<Vec<_>>();
                state.notes_failed(notes.as_slice(), block_num);
                self.state = ActorMode::AwaitingCommit(tx_id);
            },
            // Transaction execution failed.
            Err(err) => {
                tracing::error!(err = err.as_report(), "network transaction failed");
                match err {
                    NtxError::AllNotesFailed(failed) => {
                        let notes = failed.into_iter().map(|note| note.note).collect::<Vec<_>>();
                        state.notes_failed(notes.as_slice(), block_num);
                        self.state = ActorMode::AwaitingEvents;
                    },
                    NtxError::InputNotes(_)
                    | NtxError::NoteFilter(_)
                    | NtxError::Execution(_)
                    | NtxError::Proving(_)
                    | NtxError::Submission(_)
                    | NtxError::Panic(_) => {
                        self.state = ActorMode::AwaitingEvents;
                    },
                }
            },
        }
    }
}
