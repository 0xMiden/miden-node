use std::collections::{BTreeMap, BTreeSet};

use miden_objects::account::AccountId;
use miden_objects::batch::{BatchId, ProvenBatch};
use miden_objects::transaction::TransactionId;

use super::graph::{DependencyGraph, GraphError};
use super::{BlockBudget, BudgetStatus};

// BATCH GRAPH
// ================================================================================================

/// Tracks the dependencies between batches, transactions and their parents.
///
/// Batches are inserted with their transaction and parent transaction sets which defines the edges
/// of the dependency graph. Batches are initially inserted in a pending state while we wait on
/// their proofs to be generated. The dependencies are still tracked in this state.
///
/// Batches can then be promoted to ready by [submitting their proofs](Self::submit_proof) once
/// available. Proven batches are considered for inclusion in blocks once _all_ parent batches have
/// been selected.
///
/// Committed batches (i.e. included in blocks) may be [pruned](Self::prune_committed) from the
/// graph to bound the graph's size.
///
/// Batches may also be outright [purged](Self::remove_batches) from the graph. This is useful for
/// batches which may have become invalid due to external considerations e.g. expired transactions.
///
/// # Batch lifecycle
/// ```text
///                           │                           
///                     insert│                           
///                     ┌─────▼─────┐                     
///                     │  pending  ┼────┐                
///                     └─────┬─────┘    │                
///                           │          │                
///               submit_proof│          │                
///                     ┌─────▼─────┐    │                
///                     │   proved  ┼────┤                
///                     └─────┬─────┘    │                
///                           │          │                
///               select_block│          │                
///                     ┌─────▼─────┐    │                
///                     │ committed ┼────┤                
///                     └─────┬─────┘    │                
///                           │          │                
///            prune_committed│          │remove_batches
///                     ┌─────▼─────┐    │                
///                     │  <null>   ◄────┘                
///                     └───────────┘                     
/// ```
#[derive(Default, Debug, Clone, PartialEq)]
pub struct BatchGraph {
    /// Tracks the interdependencies between batches.
    inner: DependencyGraph<BatchId, ProvenBatch>,

    /// Maps each transaction to its batch, allowing for reverse lookups.
    ///
    /// Incoming batches are defined entirely in terms of transactions, including parent edges.
    /// This let's us transform these parent transactions into the relevant parent batches.
    transactions: BTreeMap<TransactionId, BatchId>,

    /// Maps each batch to its transaction set.
    ///
    /// Required because the dependency graph is defined in terms of batches. This let's us
    /// translate between batches and their transactions when required.
    batches: BTreeMap<BatchId, Vec<TransactionId>>,
}

#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum BatchInsertError {
    #[error("Transactions are already in the graph: {0:?}")]
    DuplicateTransactions(BTreeSet<TransactionId>),
    #[error("Unknown parent transaction {0}")]
    UnknownParentTransaction(TransactionId),
    #[error(transparent)]
    GraphError(#[from] GraphError<BatchId>),
}

impl BatchGraph {
    /// Inserts a new batch into the graph.
    ///
    /// Parents are the transactions on which the given transactions have a direct dependency. This
    /// includes transactions within the same batch i.e. a transaction and parent transaction may
    /// both be in this batch.
    ///
    /// # Returns
    ///
    /// The new batch's ID.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    ///   - the batch ID is already in use
    ///   - any transactions are already in the graph
    ///   - any parent transactions are _not_ in the graph
    pub fn insert(
        &mut self,
        transactions: Vec<(TransactionId, AccountId)>,
        mut parents: BTreeSet<TransactionId>,
    ) -> Result<BatchId, BatchInsertError> {
        let duplicates = transactions
            .iter()
            .filter_map(|(tx, _)| self.transactions.contains_key(tx).then_some(tx))
            .copied()
            .collect::<BTreeSet<_>>();
        if !duplicates.is_empty() {
            return Err(BatchInsertError::DuplicateTransactions(duplicates));
        }

        // Reverse lookup parent batch IDs. Take care to allow for parent transactions within this
        // batch i.e. internal dependencies.
        for (tx, _) in &transactions {
            parents.remove(tx);
        }
        let parent_batches = parents
            .into_iter()
            .map(|tx| {
                self.transactions
                    .get(&tx)
                    .copied()
                    .ok_or(BatchInsertError::UnknownParentTransaction(tx))
            })
            .collect::<Result<_, _>>()?;

        let id = BatchId::from_ids(transactions.iter().copied());
        self.inner.insert_pending(id, parent_batches)?;

        for (tx, _) in transactions.iter().copied() {
            self.transactions.insert(tx, id);
        }

        self.batches.insert(id, transactions.into_iter().map(|(tx, _)| tx).collect());

        Ok(id)
    }

    /// Removes the batches and their descendants from the graph.
    ///
    /// # Returns
    ///
    /// Returns all removed batches and their transactions.
    ///
    /// # Errors
    ///
    /// Returns an error if any of the batches are not currently in the graph.
    pub fn remove_batches(
        &mut self,
        batch_ids: BTreeSet<BatchId>,
    ) -> Result<BTreeMap<BatchId, Vec<TransactionId>>, GraphError<BatchId>> {
        // This returns all descendent batches as well.
        let batch_ids = self.inner.purge_subgraphs(batch_ids)?;

        // SAFETY: These batches must all have been inserted since they are emitted from the inner
        // dependency graph, and therefore must all be in the batches mapping.
        let batches = batch_ids
            .into_iter()
            .map(|batch_id| {
                (batch_id, self.batches.remove(&batch_id).expect("batch should be removed"))
            })
            .collect::<BTreeMap<_, _>>();

        for tx in batches.values().flatten() {
            self.transactions.remove(tx);
        }

        Ok(batches)
    }

    /// Remove all batches associated with the given transactions (and their descendents).
    ///
    /// Transactions not currently associated with a batch are allowed, but have no impact.
    ///
    /// # Returns
    ///
    /// Returns all removed batches and their transactions.
    ///
    /// Unlike [`remove_batches`](Self::remove_batches), this has no error condition as batches are
    /// derived internally.
    pub fn remove_batches_with_transactions<'a>(
        &mut self,
        txs: impl Iterator<Item = &'a TransactionId>,
    ) -> BTreeMap<BatchId, Vec<TransactionId>> {
        let batches = txs.filter_map(|tx| self.transactions.get(tx)).copied().collect();

        self.remove_batches(batches)
            .expect("batches must exist as they have been taken from internal map")
    }

    /// Removes the set of committed batches from the graph.
    ///
    /// The batches _must_ have been previously selected for inclusion in a block using
    /// [`select_block`](Self::select_block). This is intended for limiting the size of the graph by
    /// culling committed data.
    ///
    /// # Returns
    ///
    /// Returns the transactions of the pruned batches.
    ///
    /// # Errors
    ///
    /// Returns an error if
    ///   - any batch was not previously selected for inclusion in a block
    ///   - any batch is unknown
    ///   - any parent batch would be left dangling in the graph
    ///
    /// The last point implies that batches should be removed in block order.
    pub fn prune_committed(
        &mut self,
        batch_ids: &BTreeSet<BatchId>,
    ) -> Result<Vec<TransactionId>, GraphError<BatchId>> {
        // This clone could be elided by moving this call to the end. This would lose the atomic
        // property of this method though its unclear what value (if any) that has.
        self.inner.prune_processed(batch_ids.clone())?;
        let mut transactions = Vec::new();

        for batch_id in batch_ids {
            transactions.extend(self.batches.remove(batch_id).into_iter().flatten());
        }

        for tx in &transactions {
            self.transactions.remove(tx);
        }

        Ok(transactions)
    }

    /// Submits a proof for the given batch, promoting it from pending to ready for inclusion in a
    /// block once all its parents have themselves been included.
    ///
    /// # Errors
    ///
    /// Returns an error if the batch is not in the graph or if it was already previously proven.
    pub fn submit_proof(&mut self, batch: ProvenBatch) -> Result<(), GraphError<BatchId>> {
        self.inner.promote_pending(batch.id(), batch)
    }

    /// Selects the next set of batches ready for inclusion in a block while adhering to the given
    /// budget.
    ///
    /// Note that batch order should be maintained to allow for inter-batch dependencies to be
    /// correctly resolved.
    pub fn select_block(&mut self, mut budget: BlockBudget) -> Vec<ProvenBatch> {
        let mut batches = Vec::with_capacity(budget.batches);

        while let Some(batch_id) = self.inner.roots().first().copied() {
            // SAFETY: Since it was a root batch, it must definitely have a processed batch
            // associated with it.
            let batch = self.inner.get(&batch_id).expect("root should be in graph");

            // Adhere to block's budget.
            if budget.check_then_subtract(batch) == BudgetStatus::Exceeded {
                break;
            }

            // Clone is required to avoid multiple borrows of self. We delay this clone until after
            // the budget check, which is why this looks so out of place.
            let batch = batch.clone();

            // SAFETY: This is definitely a root since we just selected it from the set of roots.
            self.inner.process_root(batch_id).expect("root should be processed");

            batches.push(batch);
        }

        batches
    }

    /// Returns `true` if the graph contains the given batch.
    pub fn contains(&self, id: &BatchId) -> bool {
        self.batches.contains_key(id)
    }

    /// Returns the transactions associated with the given batch, otherwise returns `None` if the
    /// batch does not exist.
    pub fn get_transactions(&self, id: &BatchId) -> Option<&[TransactionId]> {
        self.batches.get(id).map(Vec::as_slice)
    }

    // Amount of inflight batches without a submitted proof.
    pub fn num_pending(&self) -> usize {
        self.inner.num_pending()
    }

    // Amount of inflight batches with a submitted proof.
    pub fn num_proven(&self) -> usize {
        self.inner.num_processed()
    }

    // The total number of batches in the graph.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// The amount of batches which are root nodes.
    pub fn num_roots(&self) -> usize {
        self.inner.num_roots()
    }
}

#[cfg(any(test, doctest))]
mod tests {
    use super::*;
    use crate::test_utils::Random;

    // INSERT TESTS
    // ================================================================================================

    #[test]
    fn insert_rejects_duplicate_transactions() {
        let mut rng = Random::with_random_seed();
        let tx_dup = (rng.draw_tx_id(), rng.draw_account_id());
        let tx_non_dup = (rng.draw_tx_id(), rng.draw_account_id());

        let mut uut = BatchGraph::default();

        uut.insert(vec![tx_dup], BTreeSet::default()).unwrap();
        let err = uut.insert(vec![tx_dup, tx_non_dup], BTreeSet::default()).unwrap_err();
        let expected = BatchInsertError::DuplicateTransactions([tx_dup.0].into());

        assert_eq!(err, expected);
    }

    #[test]
    fn insert_rejects_missing_parents() {
        let mut rng = Random::with_random_seed();
        let tx = (rng.draw_tx_id(), rng.draw_account_id());
        let missing = (rng.draw_tx_id(), rng.draw_account_id());

        let mut uut = BatchGraph::default();

        let err = uut.insert(vec![tx], [missing.0].into()).unwrap_err();
        let expected = BatchInsertError::UnknownParentTransaction(missing.0);

        assert_eq!(err, expected);
    }

    #[test]
    fn insert_with_internal_parent_succeeds() {
        // Ensure that a batch with internal dependencies can be inserted.
        let mut rng = Random::with_random_seed();
        let parent = (rng.draw_tx_id(), rng.draw_account_id());
        let child = (rng.draw_tx_id(), rng.draw_account_id());

        let mut uut = BatchGraph::default();
        uut.insert(vec![parent, child], [parent.0].into()).unwrap();
    }

    // PURGE_SUBGRAPHS TESTS
    // ================================================================================================

    #[test]
    fn purge_subgraphs_returns_all_purged_transaction_sets() {
        // Ensure that purge_subgraphs returns both parent and child batches when the parent is
        // pruned. Further ensure that a disjoint batch is not pruned.
        let mut rng = Random::with_random_seed();
        let parent_batch_txs =
            (0..5).map(|_| (rng.draw_tx_id(), rng.draw_account_id())).collect::<Vec<_>>();
        let child_batch_txs =
            (0..5).map(|_| (rng.draw_tx_id(), rng.draw_account_id())).collect::<Vec<_>>();
        let disjoint_batch_txs =
            (0..5).map(|_| (rng.draw_tx_id(), rng.draw_account_id())).collect();

        let mut uut = BatchGraph::default();
        let parent_batch_id = uut.insert(parent_batch_txs.clone(), BTreeSet::default()).unwrap();
        let child_batch_id =
            uut.insert(child_batch_txs.clone(), [parent_batch_txs[0].0].into()).unwrap();
        uut.insert(disjoint_batch_txs, BTreeSet::default()).unwrap();

        let result = uut.remove_batches([parent_batch_id].into()).unwrap();
        let expected = [
            (parent_batch_id, parent_batch_txs.into_iter().map(|(tx, _)| tx).collect()),
            (child_batch_id, child_batch_txs.into_iter().map(|(tx, _)| tx).collect()),
        ]
        .into();

        assert_eq!(result, expected);
    }
}
