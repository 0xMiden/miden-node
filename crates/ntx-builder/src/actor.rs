use std::time::Duration;

use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_proto::domain::mempool::MempoolEvent;
use miden_node_utils::ErrorReport;
use miden_remote_prover_client::remote_prover::tx_prover::RemoteTransactionProver;
use tokio::sync::mpsc;
use tracing::instrument;
use url::Url;

use crate::COMPONENT;
use crate::block_producer::BlockProducerClient;
use crate::state::State;
use crate::store::StoreClient;
use crate::transaction::NtxError;

#[derive(Debug, Clone)]
pub enum CoordinatorMessage {
    MempoolEvent(MempoolEvent),
}

#[derive(Debug, Clone)]
pub struct AccountActorConfig {
    pub tick_interval_ms: Duration,
    /// Address of the store gRPC server.
    pub store_url: Url,
    /// Address of the block producer gRPC server.
    pub block_producer_url: Url,
    /// Address of the remote prover. If `None`, transactions will be proven locally, which is
    /// undesirable due to the perofmrance impact.
    pub tx_prover_url: Option<Url>,
}

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
    config: AccountActorConfig,
    store: StoreClient,
    block_producer: BlockProducerClient,
    prover: Option<RemoteTransactionProver>,
}

impl AccountActor {
    fn new(
        account_prefix: NetworkAccountPrefix,
        state: State,
        coordinator_rx: mpsc::UnboundedReceiver<CoordinatorMessage>,
        config: AccountActorConfig,
    ) -> Self {
        let block_producer = BlockProducerClient::new(config.block_producer_url.clone());
        let prover = config.tx_prover_url.clone().map(RemoteTransactionProver::new);
        let store = StoreClient::new(config.store_url.clone());
        Self {
            account_prefix,
            state,
            coordinator_rx,
            config,
            store,
            block_producer,
            prover,
        }
    }

    /// Spawns the actor and returns a handle to it.
    pub fn spawn(
        account_prefix: NetworkAccountPrefix,
        state: State,
        config: AccountActorConfig,
    ) -> AccountActorHandle {
        let (coordinator_tx, coordinator_rx) = mpsc::unbounded_channel();

        let actor = AccountActor::new(account_prefix, state, coordinator_rx, config);

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
        // First catch up with all pre-existing events.
        while let Some(CoordinatorMessage::MempoolEvent(event)) = self.coordinator_rx.recv().await {
            self.state.mempool_update(event).await?;
        }

        let mut interval = tokio::time::interval(self.config.tick_interval_ms);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _next = interval.tick() => {
                    self.execute_transactions().await;
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

    async fn execute_transactions(&mut self) {
        // Select a transaction to execute.
        let Some(tx_candidate) = self.state.select_candidate(crate::MAX_NOTES_PER_TX) else {
            tracing::debug!(
                account = %self.account_prefix,
                "no candidate network transaction available");
            return;
        };
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
