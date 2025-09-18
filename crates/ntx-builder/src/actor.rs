use std::sync::Arc;

use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_proto::domain::mempool::MempoolEvent;
use miden_node_utils::ErrorReport;
use miden_objects::Word;
use miden_objects::block::BlockNumber;
use miden_remote_prover_client::remote_prover::tx_prover::RemoteTransactionProver;
use tokio::sync::{AcquireError, Semaphore, mpsc};
use url::Url;

use crate::block_producer::BlockProducerClient;
use crate::state::{State, TransactionCandidate};
use crate::transaction::NtxError;

pub enum ActorShutdownReason {
    AccountReverted(NetworkAccountPrefix),
    EventChannelClosed,
    SemaphoreFailed(AcquireError),
    CommittedBlockMismatch {
        account_prefix: NetworkAccountPrefix,
        parent_block: BlockNumber,
        current_block: BlockNumber,
    },
}

#[derive(Debug, Clone)]
pub struct AccountActorConfig {
    /// Semaphore for limiting the number of concurrent transactions across all network accounts.
    pub semaphore: Arc<Semaphore>,
    /// Address of the block producer gRPC server.
    pub block_producer_url: Url,
    /// Address of the remote prover. If `None`, transactions will be proven locally, which is
    // undesirable due to the performance impact.
    pub tx_prover_url: Option<Url>,
}

/// Account actor that manages state and processes transactions for a single network account.
pub struct AccountActor {
    event_rx: mpsc::UnboundedReceiver<MempoolEvent>,
    block_producer: BlockProducerClient,
    prover: Option<RemoteTransactionProver>,
    semaphore: Arc<Semaphore>,
}

impl AccountActor {
    pub fn new(config: &AccountActorConfig) -> (Self, mpsc::UnboundedSender<MempoolEvent>) {
        let block_producer = BlockProducerClient::new(config.block_producer_url.clone());
        let prover = config.tx_prover_url.clone().map(RemoteTransactionProver::new);
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let semaphore = config.semaphore.clone();
        let actor = Self {
            event_rx,
            block_producer,
            prover,
            semaphore,
        };
        (actor, event_tx)
    }

    pub async fn run(mut self, mut state: State) -> ActorShutdownReason {
        let semaphore = self.semaphore.clone();
        // TODO(serge): when would you wait - e.g. no notes to consume and tx in flight that you are
        // waiting on. if tx succeeded, st self to inflight tx with tx id and wait mempool
        // event to come through indicating completion. then flip back and go again. reset
        // permit to be actual permit. similarly if you failed enough times, do the same. give up on
        // account. if you receive mempool event while in state waiting for tx, unlock
        // yourself.
        use futures::FutureExt;
        // Use this to toggle between the semaphore and pending futures so that we don't thrash the
        // transaction execution flow when there is nothing to process.
        let mut toggle_fut = semaphore.acquire().boxed();
        loop {
            tokio::select! {
                event = self.event_rx.recv() => {
                    let Some(event) = event else {
                         return ActorShutdownReason::EventChannelClosed;
                    };
                    if let Some(shutdown_reason) = state.mempool_update(event).await {
                        return shutdown_reason;
                    }
                    // Re-enable the semaphore, allow transactions to be processed.
                    toggle_fut = semaphore.acquire().boxed();
                },
                permit = toggle_fut => {
                    match permit {
                        Ok(_permit) => {
                            if let Some(tx_candidate) = state.select_candidate(crate::MAX_NOTES_PER_TX) {
                                self.execute_transactions(&mut state, tx_candidate).await;
                                // TODO(serge): determine whether you need to do permit or pending state
                            }
                            // Disable the semaphore, allow events to be received.
                            toggle_fut = std::future::pending().boxed();
                        }
                        Err(err) => {
                            return ActorShutdownReason::SemaphoreFailed(err);
                        }
                    }
                }
            }
        }
    }

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
            Ok(failed) if failed.is_empty() => {},
            // Execution completed with some failed notes.
            Ok(failed) => {
                let notes = failed.into_iter().map(|note| note.note).collect::<Vec<_>>();
                state.notes_failed(notes.as_slice(), block_num);
            },
            // Transaction execution failed.
            Err(err) => {
                tracing::error!(err = err.as_report(), "network transaction failed");
                match err {
                    NtxError::AllNotesFailed(failed) => {
                        let notes = failed.into_iter().map(|note| note.note).collect::<Vec<_>>();
                        state.notes_failed(notes.as_slice(), block_num);
                    },
                    NtxError::InputNotes(_)
                    | NtxError::NoteFilter(_)
                    | NtxError::Execution(_)
                    | NtxError::Proving(_)
                    | NtxError::Submission(_)
                    | NtxError::Panic(_) => {},
                }
            },
        }
    }
}
