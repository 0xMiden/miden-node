use std::sync::Arc;

use futures::FutureExt;
use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_proto::domain::mempool::MempoolEvent;
use miden_node_utils::ErrorReport;
use miden_objects::transaction::TransactionId;
use miden_remote_prover_client::remote_prover::tx_prover::RemoteTransactionProver;
use tokio::sync::{AcquireError, RwLock, Semaphore, mpsc};
use url::Url;

use crate::block_producer::BlockProducerClient;
use crate::builder::ChainState;
use crate::state::{State, TransactionCandidate};
use crate::transaction::NtxError;

pub enum ActorShutdownReason {
    AccountReverted(NetworkAccountPrefix),
    EventChannelClosed,
    SemaphoreFailed(AcquireError),
}

#[derive(Debug, Clone)]
pub struct AccountActorConfig {
    /// Address of the block producer gRPC server.
    pub block_producer_url: Url,
    /// Address of the remote prover. If `None`, transactions will be proven locally, which is
    // undesirable due to the performance impact.
    pub tx_prover_url: Option<Url>,
    /// The latest chain state that account actors can rely on.
    pub chain_state: Arc<RwLock<ChainState>>,
}

#[derive(Default, Debug)]
enum ActorState {
    #[default]
    AwaitingEvents,
    ExecutingTransactions,
    AwaitingCommit(TransactionId),
}

/// Account actor that manages state and processes transactions for a single network account.
pub struct AccountActor {
    state: ActorState,
    event_rx: mpsc::UnboundedReceiver<MempoolEvent>,
    block_producer: BlockProducerClient,
    prover: Option<RemoteTransactionProver>,
    chain_state: Arc<RwLock<ChainState>>,
}

impl AccountActor {
    /// Constructs a new account actor and corresponding messaging channel with the given
    /// configuration.
    pub fn new(config: &AccountActorConfig) -> (Self, mpsc::UnboundedSender<MempoolEvent>) {
        let block_producer = BlockProducerClient::new(config.block_producer_url.clone());
        let prover = config.tx_prover_url.clone().map(RemoteTransactionProver::new);
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let actor = Self {
            state: ActorState::default(),
            event_rx,
            block_producer,
            prover,
            chain_state: config.chain_state.clone(),
        };
        (actor, event_tx)
    }

    /// Runs the account actor, processing events and managing state until a reason to shutdown is
    /// encountered.
    pub async fn run(mut self, mut state: State, semaphore: Arc<Semaphore>) -> ActorShutdownReason {
        // Use this to toggle between the semaphore and pending futures so that we don't thrash the
        // transaction execution flow when there is nothing to process.
        loop {
            let tx_execution_fut = match self.state {
                // Disable transaction execution.
                ActorState::AwaitingEvents | ActorState::AwaitingCommit(_) => {
                    std::future::pending().boxed()
                },
                // Enable transaction execution.
                ActorState::ExecutingTransactions => semaphore.acquire().boxed(),
            };
            tokio::select! {
                event = self.event_rx.recv() => {
                    let Some(event) = event else {
                         return ActorShutdownReason::EventChannelClosed;
                    };
                    // Re-enable transaction execution if the transaction being waited on has been
                    // committed.
                    if let ActorState::AwaitingCommit(tx_id) = self.state {
                        if let MempoolEvent::BlockCommitted { txs, .. } = &event {
                            if txs.contains(&tx_id) {
                                self.state = ActorState::ExecutingTransactions;
                            }
                        }
                    } else {
                        self.state = ActorState::ExecutingTransactions;
                    }
                    // Update state.
                    if let Some(shutdown_reason) = state.mempool_update(event).await {
                        return shutdown_reason;
                    }
                },
                permit = tx_execution_fut => {
                    match permit {
                        Ok(_permit) => {
                            // Read the chain state.
                            let chain_state = self.chain_state.read().await.clone();
                            // Find a candidate transaction and execute it.
                            if let Some(tx_candidate) = state.select_candidate(crate::MAX_NOTES_PER_TX, chain_state) {
                                self.execute_transactions(&mut state, tx_candidate).await;
                            } else {
                                // No transactions to execute, wait for events.
                                self.state = ActorState::AwaitingEvents;
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
                self.state = ActorState::AwaitingCommit(tx_id);
            },
            // Execution completed with some failed notes.
            Ok((tx_id, failed)) => {
                let notes = failed.into_iter().map(|note| note.note).collect::<Vec<_>>();
                state.notes_failed(notes.as_slice(), block_num);
                self.state = ActorState::AwaitingCommit(tx_id);
            },
            // Transaction execution failed.
            Err(err) => {
                tracing::error!(err = err.as_report(), "network transaction failed");
                match err {
                    NtxError::AllNotesFailed(failed) => {
                        let notes = failed.into_iter().map(|note| note.note).collect::<Vec<_>>();
                        state.notes_failed(notes.as_slice(), block_num);
                        self.state = ActorState::AwaitingEvents;
                    },
                    NtxError::InputNotes(_)
                    | NtxError::NoteFilter(_)
                    | NtxError::Execution(_)
                    | NtxError::Proving(_)
                    | NtxError::Submission(_)
                    | NtxError::Panic(_) => {
                        self.state = ActorState::AwaitingEvents;
                    },
                }
            },
        }
    }
}
