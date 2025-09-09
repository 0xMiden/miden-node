use std::sync::Arc;

use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_proto::domain::mempool::MempoolEvent;
use miden_node_proto::domain::note::NetworkNote;
use miden_objects::block::BlockNumber;
use miden_objects::note::Nullifier;
use miden_objects::transaction::TransactionId;
use tokio::sync::{Semaphore, mpsc};
use tracing::instrument;

use crate::COMPONENT;

use crate::state::AccountState;
use crate::transaction::NtxContext;

// ACTOR MESSAGES
// ================================================================================================

/// Messages sent from coordinator to account actors
#[derive(Debug, Clone)]
pub enum CoordinatorMessage {
    /// A mempool event that affects this account
    MempoolEvent(MempoolEvent),
    /// Periodic tick to trigger transaction processing
    ProcessTick,
}

/// Configuration for individual account actors
#[derive(Debug, Clone)]
pub struct AccountConfig {
    /// Maximum number of note execution attempts before dropping
    pub max_note_attempts: usize,
}

impl Default for AccountConfig {
    fn default() -> Self {
        Self { max_note_attempts: 10 }
    }
}

// ACCOUNT ACTOR
// ================================================================================================

/// Handle to an active account actor
pub struct ActorHandle {
    pub account_prefix: NetworkAccountPrefix,
    pub coordinator_tx: mpsc::UnboundedSender<CoordinatorMessage>,
    pub join_handle: tokio::task::JoinHandle<()>,
}

impl ActorHandle {
    /// Send a message to this actor
    pub fn send(
        &self,
        msg: CoordinatorMessage,
    ) -> Result<(), mpsc::error::SendError<CoordinatorMessage>> {
        self.coordinator_tx.send(msg)
    }

    /// Check if the actor is still running
    pub fn is_finished(&self) -> bool {
        self.join_handle.is_finished()
    }

    /// Abort the actor task
    pub fn abort(&self) {
        self.join_handle.abort();
    }
}

/// Account actor that manages state and processes transactions for a single network account
pub struct AccountActor {
    account_prefix: NetworkAccountPrefix,
    state: Option<AccountState>,
    coordinator_rx: mpsc::UnboundedReceiver<CoordinatorMessage>,
    ntx_context: NtxContext,
    rate_limiter: Arc<Semaphore>,
    config: AccountConfig,
    chain_tip_block_num: BlockNumber,
}

impl AccountActor {
    /// Creates a new account actor
    pub fn new(
        account_prefix: NetworkAccountPrefix,
        state: Option<AccountState>,
        coordinator_rx: mpsc::UnboundedReceiver<CoordinatorMessage>,
        ntx_context: NtxContext,
        rate_limiter: Arc<Semaphore>,
        config: AccountConfig,
        chain_tip_block_num: BlockNumber,
    ) -> Self {
        Self {
            account_prefix,
            state,
            coordinator_rx,
            ntx_context,
            rate_limiter,
            config,
            chain_tip_block_num,
        }
    }

    /// Spawns the actor and returns a handle to it
    pub fn spawn(
        account_prefix: NetworkAccountPrefix,
        state: Option<AccountState>,
        ntx_context: NtxContext,
        rate_limiter: Arc<Semaphore>,
        config: AccountConfig,
        chain_tip_block_num: BlockNumber,
    ) -> ActorHandle {
        let (coordinator_tx, actor_rx) = mpsc::unbounded_channel();

        let actor = AccountActor::new(
            account_prefix,
            state,
            actor_rx,
            ntx_context,
            rate_limiter,
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

        ActorHandle {
            account_prefix,
            coordinator_tx,
            join_handle,
        }
    }

    /// Main actor event loop
    #[instrument(target = COMPONENT, name = "account_actor.run", skip_all)]
    async fn run(mut self) -> anyhow::Result<()> {
        loop {
            tokio::select! {
                msg = self.coordinator_rx.recv() => {
                    match msg {
                        Some(CoordinatorMessage::MempoolEvent(event)) => {
                            if let Err(error) = self.handle_mempool_event(event) {
                                return Err(anyhow::anyhow!("failed to handle mempool event: {error}"));
                            }
                        }
                        Some(CoordinatorMessage::ProcessTick) => {
                            self.process_transactions();
                        }
                        None => {
                            return Err(anyhow::anyhow!("coordinator channel closed"));
                        }
                    }
                }
            }
        }
    }

    /// Handle a mempool event
    fn handle_mempool_event(&mut self, event: MempoolEvent) -> anyhow::Result<()> {
        match event {
            MempoolEvent::TransactionAdded {
                id,
                nullifiers,
                network_notes,
                account_delta,
            } => {
                if let Some(account_update_details) = account_delta {
                    self.apply_account_delta(account_update_details)?;
                }

                // Add any network notes that belong to this account.
                for note in network_notes {
                    if let NetworkNote::SingleTarget(note) = note {
                        if note.account_prefix() == self.account_prefix {
                            if let Some(ref mut state) = self.state {
                                state.add_note(note);
                            }
                        }
                    }
                }

                // Track nullifiers for transaction impact
                self.track_nullifiers(id, nullifiers);
            },
            MempoolEvent::BlockCommitted { header, txs } => {
                self.chain_tip_block_num = header.block_num();

                // Commit any transactions that belong to us
                for tx_id in txs {
                    self.commit_transaction(tx_id);
                }
            },
            MempoolEvent::TransactionsReverted(tx_ids) => {
                // Revert any transactions that belong to us
                for tx_id in tx_ids {
                    self.revert_transaction(tx_id);
                }
            },
        }

        // Clean up notes that have failed too many times
        if let Some(ref mut state) = self.state {
            state.drop_failing_notes(self.config.max_note_attempts);
        }

        Ok(())
    }

    /// Apply account delta from mempool event
    fn apply_account_delta(
        &mut self,
        delta: miden_objects::account::delta::AccountUpdateDetails,
    ) -> anyhow::Result<()> {
        use miden_objects::account::delta::AccountUpdateDetails;
        match delta {
            AccountUpdateDetails::New(account) => {
                // Initialize state if we don't have it yet
                if self.state.is_none() {
                    self.state = Some(AccountState::from_uncommitted_account(account));
                } else {
                    tracing::warn!(
                        account = %self.account_prefix,
                        "Received new account update for existing actor"
                    );
                }
            },
            AccountUpdateDetails::Delta(account_delta) => {
                if let Some(ref mut state) = self.state {
                    state.add_delta(&account_delta);
                }
            },
            AccountUpdateDetails::Private => {
                tracing::warn!(
                    account = %self.account_prefix,
                    "Received private account update for existing actor"
                );
            },
        }
        Ok(())
    }

    /// Track nullifiers for a transaction
    fn track_nullifiers(&mut self, _tx_id: TransactionId, nullifiers: Vec<Nullifier>) {
        // Add nullifiers to our state if they belong to our account
        if let Some(ref mut _state) = self.state {
            for nullifier in nullifiers {
                // The add_nullifier method will panic if the note doesn't exist,
                // so we need to check if it's safe to call. For now, we'll skip
                // nullifier tracking and let it be handled by transaction completion.
                // In a full implementation, we'd track which nullifiers belong to which transactions.
                tracing::debug!(
                    account = %self.account_prefix,
                    nullifier = %nullifier,
                    "Tracking nullifier for account"
                );
            }
        }
    }

    /// Commit a transaction
    fn commit_transaction(&mut self, tx_id: TransactionId) {
        // For now, we don't track transaction IDs to nullifiers mapping
        // This would be enhanced in the full implementation
        tracing::debug!(
            account = %self.account_prefix,
            tx_id = %tx_id,
            "Committing transaction"
        );

        // In a full implementation, we would:
        // 1. Look up which nullifiers belong to this transaction
        // 2. Commit those nullifiers using self.state.commit_nullifier()
        // 3. Commit any account deltas if this was our transaction
    }

    /// Revert a transaction
    fn revert_transaction(&mut self, tx_id: TransactionId) {
        tracing::debug!(
            account = %self.account_prefix,
            tx_id = %tx_id,
            "Reverting transaction"
        );

        // In a full implementation, we would:
        // 1. Look up which nullifiers belong to this transaction
        // 2. Revert those nullifiers using self.state.revert_nullifier()
        // 3. Revert any account deltas if this was our transaction
    }

    /// Process available transactions for this account
    fn process_transactions(&mut self) {
        // Check if we have state and any available notes to process
        let Some(ref state) = self.state else {
            tracing::debug!(
                account = %self.account_prefix,
                "No account state available for processing"
            );
            return;
        };

        let _available_notes: Vec<_> =
            state.available_notes(&self.chain_tip_block_num).cloned().collect();

        if _available_notes.is_empty() {
            tracing::debug!(
                account = %self.account_prefix,
                "No notes available for processing"
            );
            return;
        }

        // Try to acquire a permit for global rate limiting
        let permit = match self.rate_limiter.try_acquire() {
            Ok(permit) => permit,
            Err(_) => {
                tracing::debug!(
                    account = %self.account_prefix,
                    "Rate limit reached, skipping transaction processing"
                );
                return;
            },
        };

        // For now, skip transaction processing since we need proper chain state
        // In full implementation, coordinator would provide chain_tip_header and chain_mmr
        tracing::warn!(
            account = %self.account_prefix,
            "Skipping transaction processing - need chain state from coordinator"
        );
        drop(permit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_actor_spawn() {
        let rate_limiter = Arc::new(Semaphore::new(1));

        // Create a mock account prefix
        let account_prefix = NetworkAccountPrefix::try_from(0x10_000_000u32).unwrap();

        // Create mock NtxContext
        let ntx_context = NtxContext {
            block_producer: crate::block_producer::BlockProducerClient::new(
                "http://localhost:8080".parse().unwrap(),
            ),
            prover: None,
        };

        let config = AccountConfig::default();

        // Spawn an actor
        let _actor_handle = AccountActor::spawn(
            account_prefix,
            None, // Start with no state
            ntx_context,
            rate_limiter,
            config,
            miden_objects::block::BlockNumber::from_epoch(1),
        );
    }
}
