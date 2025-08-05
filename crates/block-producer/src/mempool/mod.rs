use std::collections::BTreeSet;
use std::sync::Arc;

use batch_graph::BatchGraph;
use graph::GraphError;
use inflight_state::InflightState;
use miden_node_proto::domain::mempool::MempoolEvent;
use miden_node_utils::tracing::OpenTelemetrySpanExt;
use miden_objects::batch::{BatchId, ProvenBatch};
use miden_objects::block::{BlockHeader, BlockNumber};
use miden_objects::transaction::TransactionId;
use miden_objects::{
    MAX_ACCOUNTS_PER_BATCH,
    MAX_INPUT_NOTES_PER_BATCH,
    MAX_OUTPUT_NOTES_PER_BATCH,
};
use subscription::SubscriptionProvider;
use tokio::sync::{Mutex, MutexGuard, mpsc};
use tracing::{instrument, warn};
use transaction_expiration::TransactionExpirations;
use transaction_graph::TransactionGraph;

use crate::domain::transaction::AuthenticatedTransaction;
use crate::errors::AddTransactionError;
use crate::{COMPONENT, DEFAULT_MAX_BATCHES_PER_BLOCK, DEFAULT_MAX_TXS_PER_BATCH};

mod batch_graph;
mod graph;
mod inflight_state;
mod subscription;
mod transaction_expiration;
mod transaction_graph;

#[cfg(test)]
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
    /// The latest inflight state of each account.
    ///
    /// Accounts without inflight transactions are not stored.
    state: InflightState,

    /// Inflight transactions.
    transactions: TransactionGraph,

    /// Tracks inflight transaction expirations.
    ///
    /// This is used to identify inflight transactions that have become invalid once their
    /// expiration block constraint has been violated. This occurs naturally as blocks get
    /// committed and the chain grows.
    expirations: TransactionExpirations,

    /// Inflight batches.
    batches: BatchGraph,

    /// The current block height of the chain.
    chain_tip: BlockNumber,

    /// The current inflight block, if any.
    block_in_progress: Option<BTreeSet<BatchId>>,

    block_budget: BlockBudget,
    batch_budget: BatchBudget,

    subscription: subscription::SubscriptionProvider,
}

// We have to implement this manually since the event's channel does not implement PartialEq.
impl PartialEq for Mempool {
    fn eq(&self, other: &Self) -> bool {
        // We use this deconstructive pattern to ensure we adapt this whenever fields are changed.
        let Self {
            state,
            transactions,
            expirations,
            batches,
            chain_tip,
            block_in_progress,
            block_budget,
            batch_budget,
            subscription: _,
        } = self;

        state == &other.state
            && transactions == &other.transactions
            && expirations == &other.expirations
            && batches == &other.batches
            && chain_tip == &other.chain_tip
            && block_in_progress == &other.block_in_progress
            && block_budget == &other.block_budget
            && batch_budget == &other.batch_budget
    }
}

impl Mempool {
    /// Creates a new [`SharedMempool`] with the provided configuration.
    pub fn shared(
        chain_tip: BlockNumber,
        batch_budget: BatchBudget,
        block_budget: BlockBudget,
        state_retention: usize,
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
        state_retention: usize,
        expiration_slack: u32,
    ) -> Mempool {
        Self {
            chain_tip,
            batch_budget,
            block_budget,
            state: InflightState::new(chain_tip, state_retention, expiration_slack),
            block_in_progress: None,
            transactions: TransactionGraph::default(),
            batches: BatchGraph::default(),
            expirations: TransactionExpirations::default(),
            subscription: SubscriptionProvider::new(chain_tip),
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
        transaction: AuthenticatedTransaction,
    ) -> Result<BlockNumber, AddTransactionError> {
        // Add transaction to inflight state.
        let parents = self.state.add_transaction(&transaction)?;
        self.subscription.transaction_added(&transaction);
        self.expirations.insert(transaction.id(), transaction.expires_at());
        self.transactions
            .insert(transaction, parents)
            .expect("Transaction should insert after passing inflight state");
        self.inject_telemetry();

        Ok(self.chain_tip)
    }

    /// Returns a set of transactions for the next batch.
    ///
    /// Transactions are returned in a valid execution ordering.
    ///
    /// Returns `None` if no transactions are available.
    #[instrument(target = COMPONENT, name = "mempool.select_batch", skip_all)]
    pub fn select_batch(&mut self) -> Option<(BatchId, Vec<AuthenticatedTransaction>)> {
        let (batch, parents) = self.transactions.select_batch(self.batch_budget);
        if batch.is_empty() {
            return None;
        }
        let tx_ids = batch.iter().map(|tx| (tx.id(), tx.account_id())).collect::<Vec<_>>();

        let batch_id = self.batches.insert(tx_ids, parents).expect("Selected batch should insert");
        self.inject_telemetry();

        Some((batch_id, batch))
    }

    /// Drops the failed batch and all of its descendants.
    ///
    /// Transactions are placed back in the queue.
    #[instrument(target = COMPONENT, name = "mempool.rollback_batch", skip_all)]
    pub fn rollback_batch(&mut self, batch: BatchId) {
        // Batch may already have been removed as part of a parent batches failure.
        if !self.batches.contains(&batch) {
            return;
        }

        let removed_batches =
            self.batches.remove_batches([batch].into()).expect("Batch was not present");

        let transactions = removed_batches.values().flatten().copied().collect();

        self.transactions
            .requeue_transactions(transactions)
            .expect("Transaction should requeue");

        tracing::warn!(
            %batch,
            descendents=?removed_batches.keys(),
            "Batch failed, dropping all inflight descendent batches, impacted transactions are back in queue."
        );
        self.inject_telemetry();
    }

    /// Marks a batch as proven if it exists.
    #[instrument(target = COMPONENT, name = "mempool.commit_batch", skip_all)]
    pub fn commit_batch(&mut self, batch: ProvenBatch) {
        // Batch may have been removed as part of a parent batches failure.
        if !self.batches.contains(&batch.id()) {
            return;
        }

        self.batches.submit_proof(batch).expect("Batch proof should submit");
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
    pub fn select_block(&mut self) -> (BlockNumber, Vec<ProvenBatch>) {
        assert!(self.block_in_progress.is_none(), "Cannot have two blocks inflight.");

        let batches = self.batches.select_block(self.block_budget);
        self.block_in_progress = Some(batches.iter().map(ProvenBatch::id).collect());
        self.inject_telemetry();

        (self.chain_tip.child(), batches)
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
    pub fn commit_block(&mut self, header: BlockHeader) {
        // Remove committed batches and transactions from graphs.
        let batches = self.block_in_progress.take().expect("No block in progress to commit");
        let transactions =
            self.batches.prune_committed(&batches).expect("Batches failed to commit");
        self.transactions
            .commit_transactions(&transactions)
            .expect("Transaction graph malformed");

        // Remove the committed transactions from expiration tracking.
        self.expirations.remove(transactions.iter());

        // Inform inflight state about committed data.
        self.state.commit_block(transactions.clone());
        self.chain_tip = self.chain_tip.child();

        self.subscription.block_committed(header, transactions);

        // Revert expired transactions and their descendents.
        self.revert_expired_transactions();
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
    pub fn rollback_block(&mut self) {
        let batches = self.block_in_progress.take().expect("No block in progress to be failed");

        // Revert all transactions. This is the nuclear (but simplest) solution.
        //
        // We currently don't have a way of determining why this block failed so take the safe route
        // and just nuke all associated transactions.
        //
        // TODO: improve this strategy, e.g. count txn failures (as well as in e.g. batch failures),
        // and only revert upon exceeding some threshold.
        let txs = batches
            .into_iter()
            .flat_map(|batch_id| {
                self.batches
                    .get_transactions(&batch_id)
                    .expect("batch from a block must be in the mempool")
            })
            .copied()
            .collect();
        self.revert_transactions(txs)
            .expect("transactions from a block must be part of the mempool");
        self.inject_telemetry();
    }

    /// Gets all transactions that expire at the new chain tip and reverts them (and their
    /// descendants) from the mempool. Returns the set of transactions that were purged.
    #[instrument(target = COMPONENT, name = "mempool.revert_expired_transactions", skip_all)]
    fn revert_expired_transactions(&mut self) -> BTreeSet<TransactionId> {
        let expired = self.expirations.get(self.chain_tip);

        self.revert_transactions(expired.iter().copied().collect())
            .expect("expired transactions must be part of the mempool");

        expired
    }

    /// Reverts the given transactions and their descendents from the mempool.
    ///
    /// This includes removing them from the transaction and batch graphs, as well as cleaning up
    /// their inflight state and expiration mappings.
    ///
    /// Transactions that were in reverted batches but that are disjoint from the reverted
    /// transactions (i.e. not descendents) are requeued and _not_ reverted.
    ///
    /// # Errors
    ///
    /// Returns an error if any transaction was not in the transaction graph i.e. if the transaction
    /// is unknown.
    #[instrument(target = COMPONENT, name = "mempool.revert_transactions", skip_all, fields(transactions.expired.ids))]
    fn revert_transactions(
        &mut self,
        txs: Vec<TransactionId>,
    ) -> Result<BTreeSet<TransactionId>, GraphError<TransactionId>> {
        tracing::Span::current().record("transactions.expired.ids", tracing::field::debug(&txs));

        // Revert all transactions and their descendents, and their associated batches.
        let reverted = self.transactions.remove_transactions(txs)?;
        let batches_reverted = self.batches.remove_batches_with_transactions(reverted.iter());

        // Requeue transactions that are disjoint from the reverted set, but were part of the
        // reverted batches.
        let to_requeue = batches_reverted
            .into_values()
            .flatten()
            .filter(|tx| !reverted.contains(tx))
            .collect();
        self.transactions
            .requeue_transactions(to_requeue)
            .expect("transactions from batches must be requeueable");

        // Cleanup state.
        self.expirations.remove(reverted.iter());
        self.state.revert_transactions(reverted.clone());

        self.subscription.txs_reverted(reverted.clone());

        Ok(reverted)
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
    fn inject_telemetry(&self) {
        let span = tracing::Span::current();

        span.set_attribute("mempool.transactions.total", self.transactions.len());
        span.set_attribute("mempool.transactions.roots", self.transactions.num_roots());
        span.set_attribute("mempool.accounts", self.state.num_accounts());
        span.set_attribute("mempool.nullifiers", self.state.num_nullifiers());
        span.set_attribute("mempool.output_notes", self.state.num_notes_created());
        span.set_attribute("mempool.batches.pending", self.batches.num_pending());
        span.set_attribute("mempool.batches.proven", self.batches.num_proven());
        span.set_attribute("mempool.batches.total", self.batches.len());
        span.set_attribute("mempool.batches.roots", self.batches.num_roots());
    }
}
