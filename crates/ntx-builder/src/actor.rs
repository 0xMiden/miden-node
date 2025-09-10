use std::sync::Arc;
use std::time::Duration;

use miden_node_proto::AccountState;
use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_proto::domain::mempool::MempoolEvent;
use miden_node_proto::domain::note::NetworkNote;
use miden_node_utils::ErrorReport;
use miden_objects::block::BlockNumber;
use miden_objects::note::Nullifier;
use miden_objects::transaction::TransactionId;
use miden_tx::FailedNote;
use tokio::sync::{Semaphore, mpsc};
use tracing::instrument;

use crate::state::State;
use crate::{COMPONENT, MAX_IN_PROGRESS_TXS};

use crate::transaction::{NtxContext, NtxError};

#[derive(Debug, Clone)]
pub enum CoordinatorMessage {
    MempoolEvent(MempoolEvent),
}

#[derive(Debug, Clone)]
pub struct AccountActorConfig {
    pub max_note_attempts: usize,
    pub tick_interval_ms: Duration,
}

impl Default for AccountActorConfig {
    fn default() -> Self {
        Self {
            max_note_attempts: 10,
            tick_interval_ms: Duration::from_millis(200),
        }
    }
}

// ACCOUNT ACTOR
// ================================================================================================

pub struct AccountActorHandle {
    pub account_prefix: NetworkAccountPrefix,
    pub coordinator_tx: mpsc::UnboundedSender<CoordinatorMessage>,
    pub join_handle: tokio::task::JoinHandle<()>,
}

impl AccountActorHandle {
    pub fn send(
        &self,
        msg: CoordinatorMessage,
    ) -> Result<(), mpsc::error::SendError<CoordinatorMessage>> {
        self.coordinator_tx.send(msg)
    }

    pub fn is_finished(&self) -> bool {
        self.join_handle.is_finished()
    }

    pub fn abort(&self) {
        self.join_handle.abort();
    }
}

/// Account actor that manages state and processes transactions for a single network account.
pub struct AccountActor {
    account_prefix: NetworkAccountPrefix,
    state: State,
    coordinator_rx: mpsc::UnboundedReceiver<CoordinatorMessage>,
    ntx_context: NtxContext,
    rate_limiter: Arc<Semaphore>,
    config: AccountActorConfig,
}

impl AccountActor {
    fn new(
        account_prefix: NetworkAccountPrefix,
        state: State,
        coordinator_rx: mpsc::UnboundedReceiver<CoordinatorMessage>,
        ntx_context: NtxContext,
        config: AccountActorConfig,
    ) -> Self {
        Self {
            account_prefix,
            state,
            coordinator_rx,
            ntx_context,
            config,
        }
    }

    /// Spawns the actor and returns a handle to it.
    pub fn spawn(
        account_prefix: NetworkAccountPrefix,
        state: State,
        ntx_context: NtxContext,
        config: AccountActorConfig,
    ) -> AccountActorHandle {
        let (coordinator_tx, coordinator_rx) = mpsc::unbounded_channel();

        let actor = AccountActor::new(
            account_prefix,
            state,
            coordinator_rx,
            ntx_context,
            config,
            chain_tip_block_num,
        );

        let join_handle = tokio::spawn(async move {
            if let Err(error) = actor.run().await {
                tracing::error!(
                    account = %account_prefix,
                    error = ?error,
                    "Account actor failed"
                );
            }
        });

        AccountActorHandle {
            account_prefix,
            coordinator_tx,
            join_handle,
        }
    }

    #[instrument(target = COMPONENT, name = "account_actor.run", skip_all)]
    async fn run(mut self) -> anyhow::Result<()> {
        let mut interval = tokio::time::interval(self.config.tick_interval_ms);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _next = interval.tick() => {
                    self.tick().await;
                },
                msg = self.coordinator_rx.recv() => {
                    match msg {
                        Some(CoordinatorMessage::MempoolEvent(event)) => {
                            if let Err(error) = self.state.mempool_update(event).await {
                                tracing::error!(
                                    account = %self.account_prefix,
                                    error = ?error,
                                    "failed to update mempool"
                                );
                            }
                        }
                        None => {
                            return Err(anyhow::anyhow!("coordinator channel closed"));
                        }
                    }
                }
            }
        }
    }

    async fn tick(&mut self) {
        let Some(candidate) = self.state.select_candidate(crate::MAX_NOTES_PER_TX) else {
            tracing::debug!(
                account = %self.account_prefix,
                "no candidate network transaction available");
            return;
        };
        let block_num = candidate.chain_tip_header.block_num();

        let execution_result = self.ntx_context.clone().execute_transaction(candidate).await;
        match execution_result {
            // Execution completed without failed notes.
            Ok(failed) if failed.is_empty() => {},
            // Execution completed with some failed notes.
            Ok(failed) => {
                let notes = failed.into_iter().map(|note| note.note).collect::<Vec<_>>();
                self.state.notes_failed(self.account_prefix, notes.as_slice(), block_num);
            },
            // Transaction execution failed.
            Err(err) => {
                tracing::warn!(err = err.as_report(), "network transaction failed");
                match err {
                    NtxError::AllNotesFailed(failed) => {
                        let notes = failed.into_iter().map(|note| note.note).collect::<Vec<_>>();
                        self.state.notes_failed(self.account_prefix, notes.as_slice(), block_num);
                    },
                    NtxError::InputNotes(_)
                    | NtxError::NoteFilter(_)
                    | NtxError::Execution(_)
                    | NtxError::Proving(_)
                    | NtxError::Submission(_)
                    | NtxError::Panic(_) => {},
                }
                self.state.candidate_failed(self.account_prefix);
            },
        }
    }
}
