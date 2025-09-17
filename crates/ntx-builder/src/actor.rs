use std::sync::Arc;

use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_proto::domain::mempool::MempoolEvent;
use miden_node_utils::ErrorReport;
use miden_objects::Word;
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
    CommittedBlockMismatch { parent_block: Word, current_block: Word },
}

#[derive(Debug, Clone)]
pub struct AccountActorConfig {
    pub semaphore: Arc<Semaphore>,
    /// Address of the block producer gRPC server.
    pub block_producer_url: Url,
    /// Address of the remote prover. If `None`, transactions will be proven locally, which is
    /// undesirable due to the perofmrance impact.
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
        loop {
            tokio::select! {
                event = self.event_rx.recv() => {
                    let Some(event) = event else {
                         return ActorShutdownReason::EventChannelClosed;
                    };
                    if let Some(shutdown_reason) = state.mempool_update(event).await {
                        return shutdown_reason;
                    }
                },
                permit = semaphore.acquire() => {
                    match permit {
                        Ok(_permit) => {
                            if let Some(tx_candidate) = state.select_candidate(crate::MAX_NOTES_PER_TX) {
                                self.execute_transactions(&mut state, tx_candidate).await;
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
