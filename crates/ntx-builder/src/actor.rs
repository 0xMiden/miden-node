use std::sync::Arc;

use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_proto::domain::mempool::MempoolEvent;
use miden_node_utils::ErrorReport;
use miden_objects::Word;
use miden_remote_prover_client::remote_prover::tx_prover::RemoteTransactionProver;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::{Semaphore, mpsc};
use url::Url;

use crate::block_producer::BlockProducerClient;
use crate::state::State;
use crate::store::{StoreClient, StoreError};
use crate::transaction::NtxError;

// ERRORS
// ================================================================================================

/// Errors that can occur during `AccountActor` execution
#[derive(Debug, thiserror::Error)]
pub enum AccountActorError {
    #[error("account does not exist: {0}")]
    AccountNotFound(NetworkAccountPrefix),

    #[error("failed to use store: {0}")]
    StoreError(#[from] Box<StoreError>),

    /// Channel to coordinator was closed
    #[error("coordinator channel closed")]
    ChannelClosed,

    /// Failed to acquire semaphore permit
    #[error("failed to acquire semaphore permit: {0}")]
    SemaphoreError(#[from] tokio::sync::AcquireError),

    #[error(
        "new block's parent commitment {parent_block} does not match local chain tip {current_block}"
    )]
    CommittedBlockMismatch { parent_block: Word, current_block: Word },

    /// Failed to update mempool state
    #[error("account creation reverted: {0}")]
    AccountCreationReverted(NetworkAccountPrefix),
}

#[derive(Debug, Clone)]
pub struct AccountActorConfig {
    pub semaphore: Arc<Semaphore>,
    /// Address of the store gRPC server.
    pub store_url: Url,
    /// Address of the block producer gRPC server.
    pub block_producer_url: Url,
    /// Address of the remote prover. If `None`, transactions will be proven locally, which is
    /// undesirable due to the perofmrance impact.
    pub tx_prover_url: Option<Url>,
}

#[derive(Clone)]
pub struct AccountActorHandle {
    pub account_prefix: NetworkAccountPrefix,
    pub event_tx: mpsc::UnboundedSender<MempoolEvent>,
}

impl AccountActorHandle {
    pub fn send(&self, msg: MempoolEvent) -> anyhow::Result<()> {
        self.event_tx.send(msg)?;
        Ok(())
    }
}

/// Account actor that manages state and processes transactions for a single network account.
pub struct AccountActor {
    account_prefix: NetworkAccountPrefix,
    store_url: Url,
    event_rx: mpsc::UnboundedReceiver<MempoolEvent>,
    block_producer: BlockProducerClient,
    prover: Option<RemoteTransactionProver>,
    semaphore: Arc<Semaphore>,
}

impl AccountActor {
    pub fn new(
        account_prefix: NetworkAccountPrefix,
        config: &AccountActorConfig,
    ) -> (Self, AccountActorHandle) {
        let block_producer = BlockProducerClient::new(config.block_producer_url.clone());
        let prover = config.tx_prover_url.clone().map(RemoteTransactionProver::new);
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let semaphore = config.semaphore.clone();
        let actor = Self {
            account_prefix,
            store_url: config.store_url.clone(),
            event_rx,
            block_producer,
            prover,
            semaphore,
        };
        let handle = AccountActorHandle { account_prefix, event_tx };
        (actor, handle)
    }

    pub async fn run(mut self) -> Result<(), AccountActorError> {
        let store = StoreClient::new(self.store_url.clone());
        let account = store.get_network_account(self.account_prefix).await.map_err(Box::new)?;
        let Some(account) = account else {
            return Err(AccountActorError::AccountNotFound(self.account_prefix));
        };
        let mut state = State::load(self.account_prefix, account, store).await.map_err(Box::new)?;

        loop {
            // First, process all available events to prevent starvation.
            loop {
                match self.event_rx.try_recv() {
                    Ok(event) => {
                        state.mempool_update(event).await?;
                        // Continue processing more events.
                    },
                    Err(TryRecvError::Empty) => {
                        // No more events available, break to execute transactions.
                        break;
                    },
                    Err(TryRecvError::Disconnected) => {
                        return Err(AccountActorError::ChannelClosed);
                    },
                }
            }

            // Acquire permit and execute transactions.
            let semaphore = self.semaphore.clone();
            let _permit = semaphore.acquire().await.map_err(AccountActorError::SemaphoreError)?;
            self.execute_transactions(&mut state).await;
        }
    }

    async fn execute_transactions(&mut self, state: &mut State) {
        // Select a transaction to execute.
        let Some(tx_candidate) = state.select_candidate(crate::MAX_NOTES_PER_TX) else {
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
