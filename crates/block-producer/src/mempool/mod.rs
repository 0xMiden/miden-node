use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use miden_node_proto::domain::mempool::MempoolEvent;
use miden_objects::batch::{BatchId, ProvenBatch};
use miden_objects::block::{BlockHeader, BlockNumber, ProvenBlock};
use miden_objects::transaction::{TransactionHeader, TransactionId};
use miden_objects::{
    MAX_ACCOUNTS_PER_BATCH,
    MAX_INPUT_NOTES_PER_BATCH,
    MAX_OUTPUT_NOTES_PER_BATCH,
};
use subscription::SubscriptionProvider;
use tokio::sync::{Mutex, MutexGuard, mpsc};
use tracing::{instrument, warn};
use transaction_expiration::TransactionExpirations;

use crate::domain::transaction::AuthenticatedTransaction;
use crate::errors::AddTransactionError;
use crate::mempool::state_dag::{NodeId, StateGraph};
use crate::{COMPONENT, DEFAULT_MAX_BATCHES_PER_BLOCK, DEFAULT_MAX_TXS_PER_BATCH};

mod batches;
mod state_dag;
mod subscription;
mod transaction_expiration;
mod transactions;

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
    /// The dependency graph formed by the currently inflight transactions, batches and blocks.
    ///
    /// This is somewhat coupled with `txs` and `batches`, and some manual work is required to keep
    /// these in sync.
    state: state_dag::StateGraph,

    /// Holds the inflight transactions and their status wrt to the state dependency DAG.
    txs: transactions::Transactions,

    /// Holds the inflight batches and their status wrt to the state dependency DAG.
    batches: batches::Batches,

    /// The current inflight block, if any.
    block_in_progress: Option<Vec<BatchId>>,

    /// The latest committed blocks.
    ///
    /// We retain `state_retention` blocks as part of our mempool state to allow some overlap with
    /// the submitted transaction and batches authentication block height. Without this,
    /// submitted data would constantly be racing against the in-flight block.
    recent_blocks: VecDeque<BlockNumber>,

    /// The latest block's number.
    chain_tip: BlockNumber,

    /// The constraints we place on every block we build.
    block_budget: BlockBudget,

    /// The constraints we place on every batch we build.
    batch_budget: BatchBudget,

    /// We reject submitted data which expires within this amount from the chain tip.
    ///
    /// This prevents wasting time on data which will likely revert before being included in a
    /// block.
    expiration_slack: u32,

    /// Blocks of committed state to retain locally. See `recent_blocks`.
    state_retention: usize,

    subscription: subscription::SubscriptionProvider,
}

// We have to implement this manually since the event's channel does not implement PartialEq.
impl PartialEq for Mempool {
    fn eq(&self, other: &Self) -> bool {
        // We use this deconstructive pattern to ensure we adapt this whenever fields are changed.
        let Self {
            state,
            batches,
            chain_tip,
            block_in_progress,
            block_budget,
            batch_budget,
            subscription: _,
            txs,
            recent_blocks,
            expiration_slack,
            state_retention,
        } = self;

        state == &other.state
            && batches == &other.batches
            && chain_tip == &other.chain_tip
            && block_in_progress == &other.block_in_progress
            && block_budget == &other.block_budget
            && batch_budget == &other.batch_budget
            && txs == &other.txs
            && recent_blocks == &other.recent_blocks
            && expiration_slack == &other.expiration_slack
            && state_retention == &other.state_retention
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
            expiration_slack,
            state_retention,
            subscription: SubscriptionProvider::new(chain_tip),
            state: StateGraph::default(),
            block_in_progress: None,
            txs: transactions::Transactions::default(),
            batches: batches::Batches::default(),
            recent_blocks: VecDeque::default(),
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
        self.expiration_check(transaction.expires_at())?;
        self.input_staleness_check(transaction.authentication_height())?;

        // Add transaction to inflight state.
        self.state.append_transaction(&transaction).expect("map err");
        self.subscription.transaction_added(&transaction);
        self.txs.insert(transaction, &self.state);

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
        let mut budget = self.batch_budget.clone();
        let mut batch = Vec::with_capacity(budget.transactions);

        loop {
            let Some(candidate) = self.txs.next_candidate() else {
                break;
            };

            if budget.check_then_subtract(candidate.get()) == BudgetStatus::Exceeded {
                break;
            }

            batch.push(candidate.get().clone());
            candidate.select(&self.state);
        }

        if batch.is_empty() {
            return None;
        }

        let batch_id = BatchId::from_ids(batch.iter().map(|tx| (tx.id(), tx.account_id())));
        self.batches.insert(batch_id, &batch);
        self.batches.check_user_batches(&self.state);
        self.state
            .insert_batch(batch_id, batch.iter().map(|tx| tx.id()))
            .expect("batch should insert as it consists of transactions selected in DAG order");

        Some((batch_id, batch))
    }

    /// Drops the failed batch and all of its descendants.
    ///
    /// Transactions are _not_ requeued.
    #[instrument(target = COMPONENT, name = "mempool.rollback_batch", skip_all)]
    pub fn rollback_batch(&mut self, batch: BatchId) {
        // Batch may already have been removed as part of a parent batch's failure.
        if !self.batches.contains_proposed(&batch) {
            return;
        }

        self.revert(batch.into());
        self.inject_telemetry();
    }

    /// Marks a batch as proven if it exists.
    #[instrument(target = COMPONENT, name = "mempool.commit_batch", skip_all)]
    pub fn commit_batch(&mut self, batch: ProvenBatch) {
        if !self.batches.contains_proposed(&batch.id()) {
            return;
        }

        self.batches.submit_proof(batch, &self.state);
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

        let mut budget = self.block_budget.clone();
        let mut block = Vec::with_capacity(budget.batches);

        loop {
            let Some(candidate) = self.batches.next_candidate() else {
                break;
            };

            if budget.check_then_subtract(candidate.get()) == BudgetStatus::Exceeded {
                break;
            }

            block.push(candidate.get().clone());
            candidate.select(&self.state);
        }

        // TODO: submit block to state
        self.block_in_progress = Some(block.iter().map(ProvenBatch::id).collect());
        self.inject_telemetry();

        (self.chain_tip.child(), block)
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
    pub fn commit_block(&mut self, block: BlockHeader) {
        let batches = self.block_in_progress.take().expect("No block in progress to commit");
        // TODO: this should become just the block number
        self.state
            .commit_block(block.block_num(), batches.iter().copied())
            .expect("block should commit since there was one in progress");
        self.recent_blocks.push_back(block.block_num());

        if self.recent_blocks.len() > self.state_retention {
            let pruned = self.recent_blocks.pop_front().unwrap();
            self.state
                .prune_block(pruned)
                .expect("pruning block should succeed as it is chronologically ordered");
        }

        for batch in batches {
            let batch = self.batches.remove(batch).expect("a committed batch must exist");
            for tx in batch.transactions().as_slice().iter().map(|tx| tx.id()) {
                self.txs.remove(tx);
            }
        }

        self.chain_tip = self.chain_tip.child();
        // FIXME: subscriptions because.. batches & transactions..

        self.revert_expired_data();

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
        assert!(self.block_in_progress.is_some(), "A block must be in progress to rollback");

        self.revert(self.chain_tip.child().into());
        self.inject_telemetry();
    }

    /// Gets all transactions that expire at the new chain tip and reverts them (and their
    /// descendants) from the mempool. Returns the set of transactions that were purged.
    #[instrument(target = COMPONENT, name = "mempool.revert_expired_data", skip_all)]
    fn revert_expired_data(&mut self) {
        todo!();
        // let expired = self.expirations.get(self.chain_tip);

        // self.revert_transactions(expired.iter().copied().collect())
        //     .expect("expired transactions must be part of the mempool");

        // expired
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

    /// Fully reverts the node and all of its ancestors.
    fn revert(&mut self, node: NodeId) {
        // Find all descendents using state DAG. We need to revert all of them, in reverse order
        // i.e. so we're only reverting a node with no ancestors. To this end, we'll track each
        // ancestors parents and children.
        let mut children = HashMap::new();
        let mut parents = HashMap::new();

        let mut to_process = vec![node];
        let mut processed = HashSet::new();
        while let Some(process) = to_process.pop() {
            if processed.contains(&process) {
                continue;
            }

            parents.insert(process, self.state.parents(process).unwrap());
            children.insert(process, self.state.children(process).unwrap());
            processed.insert(process);
        }

        while !children.is_empty() {
            let Some((&node, _)) = children.iter().find(|(_, children)| children.is_empty()) else {
                panic!("Nodes remaining while reverting but none are valid to revert next");
            };

            children.remove(&node).unwrap();
            for parent in parents.remove(&node).unwrap() {
                children.get_mut(&parent).unwrap().remove(&node);
            }

            match node {
                NodeId::Transaction(tx) => self.txs.remove(tx),
                NodeId::Batch(batch_id) => {
                    self.batches.remove(batch_id);
                },
                NodeId::Block(_) => {
                    self.block_in_progress.take();
                },
            }

            // TODO: remove from state DAG
        }
    }

    fn expiration_check(&self, expired_at: BlockNumber) -> Result<(), AddTransactionError> {
        let limit = self.chain_tip + self.expiration_slack;

        if expired_at <= limit {
            return Err(AddTransactionError::Expired { expired_at, limit });
        }

        Ok(())
    }

    fn input_staleness_check(
        &self,
        authentication_height: BlockNumber,
    ) -> Result<(), AddTransactionError> {
        let stale_limit = self.recent_blocks.front().copied().unwrap_or_default();
        if authentication_height < stale_limit {
            return Err(AddTransactionError::StaleInputs {
                input_block: authentication_height,
                stale_limit,
            });
        }

        Ok(())
    }

    /// Adds mempool stats to the current tracing span.
    ///
    /// Note that these are only visible in the OpenTelemetry context, as conventional tracing
    /// does not track fields added dynamically.
    fn inject_telemetry(&self) {
        let span = tracing::Span::current();

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
