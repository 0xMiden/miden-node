use std::collections::BTreeSet;

use miden_objects::transaction::TransactionId;

use super::{
    BatchBudget,
    BudgetStatus,
    graph::{DependencyGraph, GraphError},
};
use crate::domain::transaction::AuthenticatedTransaction;

// TRANSACTION GRAPH
// ================================================================================================

/// Tracks the dependency graph and status of transactions.
///
/// It handles insertion of transactions, locking them inqueue until they are ready to be processed.
/// A transaction is considered eligible for batch selection once all of its parents have also been
/// selected. Essentially this graph ensures that transaction dependency ordering is adhered to.
///
/// Transactions from failed batches may be [re-queued](Self::requeue_transactions) for batch
/// selection. Successful batches will eventually form part of a committed block at which point the
/// transaction data may be safely [pruned](Self::commit_transactions).
///
/// Transactions may also be outright [purged](Self::remove_transactions) from the graph. This is
/// useful for transactions which may have become invalid due to external considerations e.g.
/// expired transactions.
///
/// # Transaction lifecycle:
/// ```text
///                                        │                                   
///                                  insert│                                   
///                                  ┌─────▼─────┐                             
///                        ┌─────────►           ┼────┐                        
///                        │         └─────┬─────┘    │                        
///                        │               │          │                        
///    requeue_transactions│   select_batch│          │                        
///                        │         ┌─────▼─────┐    │                        
///                        └─────────┼ in batch  ┼────┤                        
///                                  └─────┬─────┘    │                        
///                                        │          │                        
///                     commit_transactions│          │remove_transactions     
///                                  ┌─────▼─────┐    │                        
///                                  │  <null>   ◄────┘                        
///                                  └───────────┘                             
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TransactionGraph {
    inner: DependencyGraph<TransactionId, AuthenticatedTransaction>,
}

impl TransactionGraph {
    /// Inserts a new transaction node, with edges to the given parent nodes.
    ///
    /// # Errors
    ///
    /// Follows the error conditions of [`DependencyGraph::insert_pending`].
    pub fn insert(
        &mut self,
        transaction: AuthenticatedTransaction,
        parents: BTreeSet<TransactionId>,
    ) -> Result<(), GraphError<TransactionId>> {
        self.inner.insert_pending(transaction.id(), parents)?;
        self.inner.promote_pending(transaction.id(), transaction)
    }

    /// Selects a set transactions for the next batch such that they adhere to the given budget.
    ///
    /// Also returns the transactions' parents.
    ///
    /// Internally these transactions are considered processed and cannot be emitted in future
    /// batches.
    ///
    /// Note: this may emit empty batches.
    ///
    /// See also:
    ///   - [`Self::requeue_transactions`]
    ///   - [`Self::commit_transactions`]
    pub fn select_batch(
        &mut self,
        mut budget: BatchBudget,
    ) -> (Vec<AuthenticatedTransaction>, BTreeSet<TransactionId>) {
        // This strategy just selects arbitrary roots for now. This is valid but not very
        // interesting or efficient.
        let mut batch = Vec::with_capacity(budget.transactions);
        let mut parents = BTreeSet::new();

        while let Some(root) = self.inner.roots().first().copied() {
            // SAFETY: Since it was a root batch, it must definitely have a processed batch
            // associated with it.
            let tx = self.inner.get(&root).unwrap().clone();

            // Adhere to batch budget.
            if budget.check_then_subtract(&tx) == BudgetStatus::Exceeded {
                break;
            }

            // SAFETY: This is definitely a root since we just selected it from the set of roots.
            self.inner.process_root(root).unwrap();
            let tx_parents = self.inner.parents(&root).unwrap();

            batch.push(tx);
            parents.extend(tx_parents);
        }

        (batch, parents)
    }

    /// Marks the given transactions as being back in queue.
    ///
    /// # Errors
    ///
    /// Follows the error conditions of [`DependencyGraph::revert_subgraphs`].
    pub fn requeue_transactions(
        &mut self,
        transactions: BTreeSet<TransactionId>,
    ) -> Result<(), GraphError<TransactionId>> {
        self.inner.revert_subgraphs(transactions)
    }

    /// Removes the provided transactions from the graph.
    ///
    /// # Errors
    ///
    /// Follows the error conditions of [`DependencyGraph::prune_processed`].
    pub fn commit_transactions(
        &mut self,
        tx_ids: &[TransactionId],
    ) -> Result<(), GraphError<TransactionId>> {
        // TODO: revisit this api.
        let tx_ids = tx_ids.iter().copied().collect();
        self.inner.prune_processed(tx_ids)?;
        Ok(())
    }

    /// Removes the transactions and all their descendants from the graph.
    ///
    /// Returns the removed transactions.
    ///
    /// # Errors
    ///
    /// Follows the error conditions of [`DependencyGraph::purge_subgraphs`].
    pub fn remove_transactions(
        &mut self,
        transactions: Vec<TransactionId>,
    ) -> Result<BTreeSet<TransactionId>, GraphError<TransactionId>> {
        // TODO: revisit this api.
        let transactions = transactions.into_iter().collect();
        self.inner.purge_subgraphs(transactions)
    }

    /// The total number of transactions in the graph.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// The amount of transactions which are root nodes.
    pub fn num_roots(&self) -> usize {
        self.inner.num_roots()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::mock_proven_tx;

    // BATCH SELECTION TESTS
    // ================================================================================================

    #[test]
    fn select_batch_respects_transaction_limit() {
        // These transactions are independent and just used to ensure we have more available
        // transactions than we want in the batch.
        let txs = (0..10)
            .map(|i| mock_proven_tx(i, vec![], vec![]))
            .map(AuthenticatedTransaction::from_inner);

        let mut uut = TransactionGraph::default();
        for tx in txs {
            uut.insert(tx, [].into()).unwrap();
        }

        let (batch, parents) =
            uut.select_batch(BatchBudget { transactions: 0, ..Default::default() });
        assert!(batch.is_empty());
        assert!(parents.is_empty());

        let (batch, parents) =
            uut.select_batch(BatchBudget { transactions: 3, ..Default::default() });
        assert_eq!(batch.len(), 3);
        assert!(parents.is_empty());

        let (batch, parents) =
            uut.select_batch(BatchBudget { transactions: 4, ..Default::default() });
        assert_eq!(batch.len(), 4);
        assert!(parents.is_empty());

        // We expect this to be partially filled.
        let (batch, parents) =
            uut.select_batch(BatchBudget { transactions: 4, ..Default::default() });
        assert_eq!(batch.len(), 3);
        assert!(parents.is_empty());

        // And thereafter empty.
        let (batch, parents) =
            uut.select_batch(BatchBudget { transactions: 100, ..Default::default() });
        assert!(batch.is_empty());
        assert!(parents.is_empty());
    }
}
