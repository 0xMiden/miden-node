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
    state: state_dag::StateGraph,

    txs: transactions::Transactions,

    user_batches: HashMap<BatchId, ProvenBatch>,

    proven_batches: HashMap<BatchId, ProvenBatch>,

    batches_queued: HashSet<BatchId>,
    batches_selectable: HashSet<BatchId>,
    batches_inprogress: HashMap<BatchId, Vec<TransactionId>>,

    // Batches can be
    // - user (but this is separate)
    // - in-progress
    // - in-queue
    // - in-block
    /// The current block height of the chain.
    chain_tip: BlockNumber,
    expiration_slack: u32,

    /// The current inflight block, if any.
    block_in_progress: Option<BTreeSet<BatchId>>,
    recent_blocks: VecDeque<ProvenBlock>,

    block_budget: BlockBudget,
    batch_budget: BatchBudget,

    subscription: subscription::SubscriptionProvider,
}

// We have to implement this manually since the event's channel does not implement PartialEq.
impl PartialEq for Mempool {
    fn eq(&self, other: &Self) -> bool {
        // We use this deconstructive pattern to ensure we adapt this whenever fields are changed.
        // let Self {
        //     state,
        //     transactions,
        //     expirations,
        //     batches,
        //     chain_tip,
        //     block_in_progress,
        //     block_budget,
        //     batch_budget,
        //     subscription: _,
        //     selectable_transactions,
        // } = self;

        // state == &other.state
        //     && transactions == &other.transactions
        //     && expirations == &other.expirations
        //     && batches == &other.batches
        //     && chain_tip == &other.chain_tip
        //     && block_in_progress == &other.block_in_progress
        //     && block_budget == &other.block_budget
        //     && batch_budget == &other.batch_budget
        todo!();
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
        // Self {
        //     chain_tip,
        //     batch_budget,
        //     block_budget,
        //     state: StateGraph::default(),
        //     block_in_progress: None,
        //     expirations: TransactionExpirations::default(),
        //     subscription: SubscriptionProvider::new(chain_tip),
        //     transactions: HashMap::default(),
        //     proven_batches: HashMap::default(),
        //     selected_batches: HashMap::default(),
        //     selectable_transactions: HashSet::default(),
        //     selected_transactions: HashSet::default(),
        //     selectable_batches: HashSet::default(),
        //     expiration_slack,
        //     recent_blocks: todo!(),
        // }
        todo!()
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

        let mut user_batches = HashSet::new();

        loop {
            let Some(candidate) = self.txs.next_candidate() else {
                break;
            };

            // Adhere to batch budget.
            if budget.check_then_subtract(candidate.get()) == BudgetStatus::Exceeded {
                break;
            }

            batch.push(candidate.get().clone());
            user_batches.extend(candidate.select(&self.state));
        }

        self.user_batch_check(user_batches);

        if batch.is_empty() {
            return None;
        }

        let batch_id = BatchId::from_ids(batch.iter().map(|tx| (tx.id(), tx.account_id())));
        let txs = batch.iter().map(AuthenticatedTransaction::id).collect();

        self.batches_inprogress.insert(batch_id, txs);

        Some((batch_id, batch))
    }

    /// Drops the failed batch and all of its descendants.
    ///
    /// Transactions are placed back in the queue.
    #[instrument(target = COMPONENT, name = "mempool.rollback_batch", skip_all)]
    pub fn rollback_batch(&mut self, batch: BatchId) {
        // // Batch may already have been removed as part of a parent batches failure.
        // let Some(txs) = self.batches_inprogress.remove(&batch) else {
        //     return;
        // };

        // let removed_batches = self
        //     .proven_batches
        //     .remove_batches([batch].into())
        //     .expect("Batch was not present");

        // let transactions = removed_batches.values().flatten().copied().collect();

        // self.transactions
        //     .requeue_transactions(transactions)
        //     .expect("Transaction should requeue");

        // tracing::warn!(
        //     %batch,
        //     descendents=?removed_batches.keys(),
        //     "Batch failed, dropping all inflight descendent batches, impacted transactions are
        // back in queue." );
        self.inject_telemetry();
    }

    /// Marks a batch as proven if it exists.
    #[instrument(target = COMPONENT, name = "mempool.commit_batch", skip_all)]
    pub fn commit_batch(&mut self, batch: ProvenBatch) {
        let id = batch.id();

        // Batches can be invalidated if their ancestors are reverted.
        if self.batches_inprogress.remove(&batch.id()).is_none() {
            return;
        }

        self.state.insert_batch(
            batch.id(),
            batch.transactions().as_slice().iter().map(TransactionHeader::id),
        )
        .expect("this is an inflight batch who's transactions should therefore all be in the state DAG");

        self.proven_batches.insert(id, batch);
        self.batch_selection_check(id);

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
        let mut batch = Vec::with_capacity(budget.batches);

        loop {
            //     // Select an arbitrary available transaction for now. This would be a place to
            // order     // by fees or some other strategy.
            //     let Some(candidate) = self.selectable_transactions.iter().next() else {
            //         break;
            //     };

            //     let tx = self
            //         .transactions
            //         .get(&candidate)
            //         .expect("A selectable transaction's data should exist in the mempool");

            //     // Adhere to batch budget.
            //     if budget.check_then_subtract(tx) == BudgetStatus::Exceeded {
            //         break;
            //     }

            //     batch.push(tx.clone());
            //     self.selectable_transactions.remove(&tx.id());
            //     self.selected_transactions.insert(tx.id());

            //     let children = self.state.children(tx);
            //     for child in children {
            //         if let NodeId::Transaction(child) = child {
            //             self.tx_selection_check(&child);
            //         }
            //     }
        }

        // if batch.is_empty() {
        //     return None;
        // }

        // let batch_id = BatchId::from_ids(batch.iter().map(|tx| (tx.id(), tx.account_id())));

        // // Update the graph by replacing the tx nodes with the single new batch node.
        // self.state
        //     .replace_unchecked(NodeId::Batch(batch_id), &batch)
        //     .expect("this is well formed");
        // self.selected_batches
        //     .insert(batch_id, batch.iter().map(AuthenticatedTransaction::id).collect());

        // Some((batch_id, batch))

        // self.block_in_progress = Some(batches.iter().map(ProvenBatch::id).collect());
        // self.inject_telemetry();

        // (self.chain_tip.child(), batches)
        todo!();
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
    pub fn commit_block(&mut self, block: ProvenBlock) {
        // Mark data as committed in the DAG.
        // Remove batch and transaction data as it is no longer required.
        // Update recent history.
        // Revert expired data.

        let batches = self.block_in_progress.take().expect("No block in progress to commit");

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
                self.proven_batches
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
        let reverted = self.submitted_transactions.remove_transactions(txs)?;
        let batches_reverted =
            self.proven_batches.remove_batches_with_transactions(reverted.iter());

        // Requeue transactions that are disjoint from the reverted set, but were part of the
        // reverted batches.
        let to_requeue = batches_reverted
            .into_values()
            .flatten()
            .filter(|tx| !reverted.contains(tx))
            .collect();
        self.submitted_transactions
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

    fn tx_selection_check(&mut self, tx: TransactionId) {
        assert!(
            !self.txs_queued.contains(&tx),
            "selection check for transaction {tx} which is already marked as processed"
        );

        let parents = self.state.parents(tx).expect(
            "selection check should only occur for transactions which are in the state DAG",
        );

        // A transaction becomes selectable once _all_ its parents have been selected.
        //
        // Note that this requirement also extends to user submitted batches since these can build
        // on unbatched transactions and are therefore "blocked" as well.
        for parent in parents {
            match parent {
                NodeId::Transaction(parent) if self.txs_queued.contains(&parent) => return,
                NodeId::Batch(parent) if self.user_batches.contains(&parent) => return,
                _ => {},
            }
        }

        self.txs_selectable.insert(tx);
    }

    fn batch_selection_check(&mut self, batch: BatchId) {
        // assert!(
        //     !self.txs_queued.contains(&tx),
        //     "selection check for transaction {tx} which is already marked as processed"
        // );

        let parents = self
            .state
            .parents(batch)
            .expect("selection check should only occur for batches which are in the state DAG");

        // A batch is selectable once all of its parents have been included in a block.
        for parent in parents {
            match parent {
                NodeId::Transaction(_) => return,
                NodeId::Batch(parent) if self.user_batches.contains(&parent) => return,
                NodeId::Batch(parent) if self.batches_queued.contains(&parent) => return,
                _ => {},
            }
        }

        self.batches_selectable.insert(batch);
    }

    fn user_batch_check(&mut self, mut to_check: HashSet<BatchId>) {
        'outer: while let Some(batch) = to_check.iter().next().copied() {
            to_check.remove(&batch);

            let parents = self.state.parents(batch).expect(
                "user batch check should only occur for batches which are in the state DAG",
            );
            for parent in parents {
                match parent {
                    NodeId::Transaction(_) => continue 'outer,
                    NodeId::Batch(parent) if self.user_batches.contains(&parent) => {
                        continue 'outer;
                    },
                    _ => {},
                }
            }

            // Promoting this user batch could unlock child batches, so add them to the list.
            let children = self.state.children(batch).expect(
                "user batch check should only occur for batches which are in the state DAG",
            );
            for child in children {
                if let NodeId::Batch(child) = child {
                    to_check.insert(child);
                }
            }

            self.user_batches.remove(&batch);
            self.batch_selection_check(batch);
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
        let stale_limit = self
            .recent_blocks
            .front()
            .map(|block| block.header().block_num())
            .unwrap_or_default();
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
