use std::num::NonZeroUsize;
use std::sync::Arc;

use miden_node_proto::domain::mempool::MempoolEvent;
use miden_objects::batch::{BatchId, ProvenBatch};
use miden_objects::block::BlockNumber;
use miden_objects::{
    MAX_ACCOUNTS_PER_BATCH,
    MAX_INPUT_NOTES_PER_BATCH,
    MAX_OUTPUT_NOTES_PER_BATCH,
};
use subscription::SubscriptionProvider;
use tokio::sync::{Mutex, MutexGuard, mpsc};
use tracing::{instrument, warn};

use crate::domain::transaction::AuthenticatedTransaction;
use crate::errors::AddTransactionError;
use crate::{COMPONENT, DEFAULT_MAX_BATCHES_PER_BLOCK, DEFAULT_MAX_TXS_PER_BATCH};

mod state_dag;
mod subscription;

// FIXME(Mirko): Re-enable these once mempool refactor is completed.
#[cfg(all(false, test))]
mod tests;

// MEMPOOL BUDGET
// ================================================================================================

/// Limits placed on a batch's contents.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BatchBudget {
    /// Maximum number of transactions allowed in a batch.
    pub transactions: usize,
    /// Maximum number of input notes allowed.
    pub input_notes: usize,
    /// Maximum number of output notes allowed.
    pub output_notes: usize,
    /// Maximum number of updated accounts.
    pub accounts: usize,
}

/// Limits placed on a blocks's contents.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BlockBudget {
    /// Maximum number of batches allowed in a block.
    pub batches: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BudgetStatus {
    /// The operation remained within the budget.
    WithinScope,
    /// The operation exceeded the budget.
    Exceeded,
}

impl Default for BatchBudget {
    fn default() -> Self {
        Self {
            transactions: DEFAULT_MAX_TXS_PER_BATCH,
            input_notes: MAX_INPUT_NOTES_PER_BATCH,
            output_notes: MAX_OUTPUT_NOTES_PER_BATCH,
            accounts: MAX_ACCOUNTS_PER_BATCH,
        }
    }
}

impl Default for BlockBudget {
    fn default() -> Self {
        Self { batches: DEFAULT_MAX_BATCHES_PER_BLOCK }
    }
}

impl BatchBudget {
    /// Attempts to consume the transaction's resources from the budget.
    ///
    /// Returns [`BudgetStatus::Exceeded`] if the transaction would exceed the remaining budget,
    /// otherwise returns [`BudgetStatus::Ok`] and subtracts the resources from the budger.
    #[must_use]
    fn check_then_subtract(&mut self, tx: &AuthenticatedTransaction) -> BudgetStatus {
        // This type assertion reminds us to update the account check if we ever support multiple
        // account updates per tx.
        const ACCOUNT_UPDATES_PER_TX: usize = 1;
        let _: miden_objects::account::AccountId = tx.account_update().account_id();

        let output_notes = tx.output_note_count();
        let input_notes = tx.input_note_count();

        if self.transactions == 0
            || self.accounts < ACCOUNT_UPDATES_PER_TX
            || self.input_notes < input_notes
            || self.output_notes < output_notes
        {
            return BudgetStatus::Exceeded;
        }

        self.transactions -= 1;
        self.accounts -= ACCOUNT_UPDATES_PER_TX;
        self.input_notes -= input_notes;
        self.output_notes -= output_notes;

        BudgetStatus::WithinScope
    }
}

impl BlockBudget {
    /// Attempts to consume the batch's resources from the budget.
    ///
    /// Returns [`BudgetStatus::Exceeded`] if the batch would exceed the remaining budget,
    /// otherwise returns [`BudgetStatus::Ok`].
    #[must_use]
    fn check_then_subtract(&mut self, _batch: &ProvenBatch) -> BudgetStatus {
        if self.batches == 0 {
            BudgetStatus::Exceeded
        } else {
            self.batches -= 1;
            BudgetStatus::WithinScope
        }
    }
}

// MEMPOOL
// ================================================================================================

#[derive(Clone)]
pub struct SharedMempool(Arc<Mutex<Mempool>>);

impl SharedMempool {
    #[instrument(target = COMPONENT, name = "mempool.lock", skip_all)]
    pub async fn lock(&self) -> MutexGuard<'_, Mempool> {
        self.0.lock().await
    }
}

#[derive(Clone, Debug)]
pub struct Mempool {
    state_dag: state_dag::StateDag,

    /// The constraints we place on every block we build.
    block_budget: BlockBudget,

    /// The constraints we place on every batch we build.
    batch_budget: BatchBudget,

    subscription: subscription::SubscriptionProvider,
}

// We have to implement this manually since the event's channel does not implement PartialEq.
impl PartialEq for Mempool {
    fn eq(&self, other: &Self) -> bool {
        self.state_dag == other.state_dag
    }
}

impl Mempool {
    /// Creates a new [`SharedMempool`] with the provided configuration.
    pub fn shared(
        chain_tip: BlockNumber,
        batch_budget: BatchBudget,
        block_budget: BlockBudget,
        state_retention: NonZeroUsize,
        expiration_slack: u32,
    ) -> SharedMempool {
        SharedMempool(Arc::new(Mutex::new(Self::new(
            chain_tip,
            batch_budget,
            block_budget,
            state_retention,
            expiration_slack,
        ))))
    }

    fn new(
        chain_tip: BlockNumber,
        batch_budget: BatchBudget,
        block_budget: BlockBudget,
        state_retention: NonZeroUsize,
        expiration_slack: u32,
    ) -> Mempool {
        Self {
            batch_budget,
            block_budget,
            subscription: SubscriptionProvider::new(chain_tip),
            state_dag: state_dag::StateDag::new(chain_tip, state_retention, expiration_slack),
        }
    }

    /// Adds a transaction to the mempool.
    ///
    /// Sends a [`MempoolEvent::TransactionAdded`] event to subscribers.
    ///
    /// # Returns
    ///
    /// Returns the current block height.
    ///
    /// # Errors
    ///
    /// Returns an error if the transaction's initial conditions don't match the current state.
    #[instrument(target = COMPONENT, name = "mempool.add_transaction", skip_all, fields(tx=%transaction.id()))]
    pub fn add_transaction(
        &mut self,
        transaction: Arc<AuthenticatedTransaction>,
    ) -> Result<BlockNumber, AddTransactionError> {
        self.state_dag.append_transaction(transaction)?;

        self.inject_telemetry();

        Ok(self.state_dag.chain_tip())
    }

    /// Returns a set of transactions for the next batch.
    ///
    /// Transactions are returned in a valid execution ordering.
    ///
    /// Returns `None` if no transactions are available.
    #[instrument(target = COMPONENT, name = "mempool.select_batch", skip_all)]
    pub fn select_batch(&mut self) -> Option<(BatchId, Vec<Arc<AuthenticatedTransaction>>)> {
        let result = self.state_dag.propose_batch(self.batch_budget);
        self.inject_telemetry();
        result
    }

    /// Drops the failed batch and all of its descendants.
    ///
    /// Transactions are _not_ requeued.
    #[instrument(target = COMPONENT, name = "mempool.rollback_batch", skip_all)]
    pub fn rollback_batch(&mut self, batch: BatchId) {
        self.state_dag.revert_proposed_batch(batch);
        self.inject_telemetry();
    }

    /// Marks a batch as proven if it exists.
    #[instrument(target = COMPONENT, name = "mempool.commit_batch", skip_all)]
    pub fn commit_batch(&mut self, batch: Arc<ProvenBatch>) {
        self.state_dag.submit_batch_proof(batch);
        self.inject_telemetry();
    }

    /// Select batches for the next block.
    ///
    /// Note that the set of batches
    /// - may be empty if none are available, and
    /// - may contain dependencies and therefore the order must be maintained
    ///
    /// # Panics
    ///
    /// Panics if there is already a block in flight.
    #[instrument(target = COMPONENT, name = "mempool.select_block", skip_all)]
    pub fn select_block(&mut self) -> (BlockNumber, Vec<Arc<ProvenBatch>>) {
        let result = self.state_dag.propose_block(self.block_budget);
        self.inject_telemetry();
        result
    }

    /// Notify the pool that the in flight block was successfully committed to the chain.
    ///
    /// The pool will mark the associated batches and transactions as committed, and prune stale
    /// committed data, and purge transactions that are now considered expired.
    ///
    /// Sends a [`MempoolEvent::BlockCommitted`] event to subscribers, as well as a
    /// [`MempoolEvent::TransactionsReverted`] for transactions that are now considered expired.
    ///
    /// # Returns
    ///
    /// Returns a set of transactions that were purged from the mempool because they can no longer
    /// be included in in the chain (e.g., expired transactions and their descendants).
    ///
    /// # Panics
    ///
    /// Panics if there is no block in flight.
    #[instrument(target = COMPONENT, name = "mempool.commit_block", skip_all)]
    pub fn commit_block(&mut self, block: BlockNumber) {
        self.state_dag.commit_proposed_block(block);
        self.inject_telemetry();
    }

    /// Notify the pool that construction of the in flight block failed.
    ///
    /// The pool will purge the block and all of its contents from the pool.
    ///
    /// Sends a [`MempoolEvent::TransactionsReverted`] event to subscribers.
    ///
    /// # Returns
    ///
    /// Returns a set of transaction IDs that were reverted because they can no longer be
    /// included in in the chain (e.g., expired transactions and their descendants)
    ///
    /// # Panics
    ///
    /// Panics if there is no block in flight.
    #[instrument(target = COMPONENT, name = "mempool.rollback_block", skip_all)]
    pub fn rollback_block(&mut self, block: BlockNumber) {
        self.state_dag.revert_proposed_block(block);
        self.inject_telemetry();
    }

    /// Creates a subscription to [`MempoolEvent`] which will be emitted in the order they occur.
    ///
    /// Only emits events which occurred after the current committed block.
    ///
    /// # Errors
    ///
    /// Returns an error if the provided chain tip does not match the mempool's chain tip. This
    /// prevents desync between the caller's view of the world and the mempool's event stream.
    #[instrument(target = COMPONENT, name = "mempool.subscribe", skip_all)]
    pub fn subscribe(
        &mut self,
        chain_tip: BlockNumber,
    ) -> Result<mpsc::Receiver<MempoolEvent>, BlockNumber> {
        self.subscription.subscribe(chain_tip)
    }

    /// Adds mempool stats to the current tracing span.
    ///
    /// Note that these are only visible in the OpenTelemetry context, as conventional tracing
    /// does not track fields added dynamically.
    #[allow(clippy::unused_self, reason = "wip: mempool refactor")]
    fn inject_telemetry(&self) {
        let _span = tracing::Span::current();

        // span.set_attribute("mempool.transactions.total", self.transactions.len());
        // span.set_attribute("mempool.transactions.roots", self.transactions.num_roots());
        // span.set_attribute("mempool.accounts", self.state.num_accounts());
        // span.set_attribute("mempool.nullifiers", self.state.num_nullifiers());
        // span.set_attribute("mempool.output_notes", self.state.num_notes_created());
        // span.set_attribute("mempool.batches.pending", self.batches.num_pending());
        // span.set_attribute("mempool.batches.proven", self.batches.num_proven());
        // span.set_attribute("mempool.batches.total", self.batches.len());
        // span.set_attribute("mempool.batches.roots", self.batches.num_roots());
    }
}
