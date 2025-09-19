use std::num::NonZeroUsize;
use std::sync::Arc;

use miden_node_proto::domain::mempool::MempoolEvent;
use miden_objects::batch::{BatchId, ProvenBatch};
use miden_objects::block::BlockNumber;
use subscription::SubscriptionProvider;
use tokio::sync::{Mutex, MutexGuard, mpsc};
use tracing::{instrument, warn};

use crate::domain::transaction::AuthenticatedTransaction;
use crate::errors::{AddTransactionError, VerifyTxError};
use crate::mempool::budget::BudgetStatus;
use crate::mempool::nodes::{
    BlockNode,
    NodeId,
    ProposedBatchNode,
    ProvenBatchNode,
    TransactionNode,
};
use crate::{COMPONENT, SERVER_MEMPOOL_EXPIRATION_SLACK, SERVER_MEMPOOL_STATE_RETENTION};

mod budget;
pub use budget::{BatchBudget, BlockBudget};

mod nodes;
mod state;
mod subscription;

// FIXME(Mirko): Re-enable these once mempool refactor is completed.
#[cfg(all(false, test))]
mod tests;

#[derive(Clone)]
pub struct SharedMempool(Arc<Mutex<Mempool>>);

#[derive(Debug, Clone)]
pub struct MempoolConfig {
    pub block_budget: BlockBudget,
    pub batch_budget: BatchBudget,
    pub expiration_slack: u32,
    pub state_retention: NonZeroUsize,
}

impl Default for MempoolConfig {
    fn default() -> Self {
        Self {
            block_budget: BlockBudget::default(),
            batch_budget: BatchBudget::default(),
            expiration_slack: SERVER_MEMPOOL_EXPIRATION_SLACK,
            state_retention: SERVER_MEMPOOL_STATE_RETENTION,
        }
    }
}

impl SharedMempool {
    #[instrument(target = COMPONENT, name = "mempool.lock", skip_all)]
    pub async fn lock(&self) -> MutexGuard<'_, Mempool> {
        self.0.lock().await
    }
}

#[derive(Clone, Debug)]
pub struct Mempool {
    /// Contains the aggregated state of all transactions, batches and blocks currently inflight in
    /// the mempool. Combines with `nodes` to describe the mempool's state graph.
    state: state::InflightState,

    /// Contains all the transactions, batches and blocks currently in the mempool.
    nodes: nodes::Nodes,

    chain_tip: BlockNumber,

    config: MempoolConfig,
    subscription: subscription::SubscriptionProvider,
}

// We have to implement this manually since the subscription channel does not implement PartialEq.
impl PartialEq for Mempool {
    fn eq(&self, other: &Self) -> bool {
        self.state == other.state && self.nodes == other.nodes
    }
}

impl Mempool {
    /// Creates a new [`SharedMempool`] with the provided configuration.
    pub fn shared(chain_tip: BlockNumber, config: MempoolConfig) -> SharedMempool {
        SharedMempool(Arc::new(Mutex::new(Self::new(chain_tip, config))))
    }

    fn new(chain_tip: BlockNumber, config: MempoolConfig) -> Mempool {
        Self {
            config,
            chain_tip,
            subscription: SubscriptionProvider::new(chain_tip),
            state: state::InflightState::default(),
            nodes: nodes::Nodes::default(),
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
    #[instrument(target = COMPONENT, name = "mempool.add_transaction", skip_all, fields(tx=%tx.id()))]
    pub fn add_transaction(
        &mut self,
        tx: Arc<AuthenticatedTransaction>,
    ) -> Result<BlockNumber, AddTransactionError> {
        self.authentication_staleness_check(tx.authentication_height())?;
        self.expiration_check(tx.expires_at())?;

        // The transaction should append to the existing mempool state.
        let account_commitment = self
            .state
            .account_commitment(&tx.account_id())
            .or(tx.store_account_state())
            .unwrap_or_default();
        if tx.account_update().initial_state_commitment() != account_commitment {
            return Err(VerifyTxError::IncorrectAccountInitialCommitment {
                tx_initial_account_commitment: tx.account_update().initial_state_commitment(),
                current_account_commitment: account_commitment,
            }
            .into());
        }
        let double_spend = self.state.nullifiers_exist(tx.nullifiers());
        if !double_spend.is_empty() {
            return Err(VerifyTxError::InputNotesAlreadyConsumed(double_spend).into());
        }
        let duplicates = self.state.output_notes_exist(tx.output_note_ids());
        if !duplicates.is_empty() {
            return Err(VerifyTxError::OutputNotesAlreadyExist(duplicates).into());
        }

        // Insert the transaction node.
        let tx_id = tx.id();
        let node_id = NodeId::Transaction(tx.id());
        let node = TransactionNode(tx);

        self.state.insert(node_id, &node);
        self.nodes.txs.insert(tx_id, node);

        self.inject_telemetry();

        Ok(self.chain_tip)
    }

    /// Returns a set of transactions for the next batch.
    ///
    /// Transactions are returned in a valid execution ordering.
    ///
    /// Returns `None` if no transactions are available.
    #[instrument(target = COMPONENT, name = "mempool.select_batch", skip_all)]
    pub fn select_batch(&mut self) -> Option<(BatchId, Vec<Arc<AuthenticatedTransaction>>)> {
        // The selection algorithm is fairly neanderthal in nature.
        //
        // We iterate over all transaction nodes, each time selecting the first transaction which
        // has no parent nodes that are unselected transactions. This is fairly primitive, but
        // avoids the manual bookkeeping of which transactions are selectable.
        //
        // This is still reasonably performant given that we only retain unselected transactions as
        // transaction nodes i.e. selected transactions become batch nodes.
        //
        // The additional bookkeeping can be implemented once we have fee related strategies. KISS.

        let mut selected = Vec::new();
        let mut budget = self.config.batch_budget;

        'outer: loop {
            'next_candidate: for candidate in self.nodes.txs.values() {
                if selected.contains(&candidate.0.id()) {
                    continue;
                }

                let parents = self.state.parents(candidate);
                for parent in parents {
                    match parent {
                        NodeId::Transaction(parent) if !selected.contains(&parent) => {
                            continue 'next_candidate;
                        },
                        _ => {},
                    }
                }

                if budget.check_then_subtract(&candidate.0) == BudgetStatus::Exceeded {
                    break 'outer;
                }

                selected.push(candidate.0.id());
                continue 'outer;
            }

            break 'outer;
        }

        if selected.is_empty() {
            return None;
        }

        // Assemble batch and update internal state.
        let mut batch_node = ProposedBatchNode(vec![]);
        for tx in selected {
            // SAFETY: Selected txs come from the transaction pool and are unique.
            let tx = self.nodes.txs.remove(&tx).unwrap();
            self.state.remove(&tx);
            batch_node.0.push(tx);
        }

        let batch_id =
            BatchId::from_transactions(batch_node.0.iter().map(|tx| tx.0.raw_proven_transaction()));
        let batch = batch_node.0.iter().map(|tx| Arc::clone(&tx.0)).collect();

        self.state.insert(NodeId::ProposedBatch(batch_id), &batch_node);
        self.nodes.proposed_batches.insert(batch_id, batch_node);

        self.inject_telemetry();
        Some((batch_id, batch))
    }

    /// Drops the failed batch and all of its descendants.
    ///
    /// Transactions are _not_ requeued.
    #[allow(clippy::unused_self, reason = "wip")]
    #[instrument(target = COMPONENT, name = "mempool.rollback_batch", skip_all)]
    pub fn rollback_batch(&mut self, _batch: BatchId) {
        todo!();
        // self.inject_telemetry();
    }

    /// Marks a batch as proven if it exists.
    #[instrument(target = COMPONENT, name = "mempool.commit_batch", skip_all)]
    pub fn commit_batch(&mut self, batch: Arc<ProvenBatch>) {
        // Due to the distributed nature of the system, its possible that a proposed batch was
        // already proven, or already reverted. This guards against this eventuality.
        let Some(proposed) = self.nodes.proposed_batches.remove(&batch.id()) else {
            return;
        };

        let batch_id = batch.id();
        self.state.remove(&proposed);

        let node = ProvenBatchNode { txs: proposed.0, inner: batch };
        self.state.insert(NodeId::ProvenBatch(batch_id), &node);
        self.nodes.proven_batches.insert(batch_id, node);

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
        assert!(
            self.nodes.proposed_block.is_none(),
            "block {} is already in progress",
            self.nodes.proposed_block.as_ref().unwrap().0
        );

        let mut selected = Vec::new();
        let mut budget = self.config.block_budget;

        'outer: loop {
            'next_candidate: for candidate in self.nodes.proven_batches.values() {
                if selected.contains(&candidate.inner.id()) {
                    continue;
                }

                let parents = self.state.parents(candidate);
                for parent in parents {
                    match parent {
                        NodeId::ProvenBatch(batch) if selected.contains(&batch) => {},
                        NodeId::Block(_) => {},
                        _ => continue 'next_candidate,
                    }
                }

                if budget.check_then_subtract(&candidate.inner) == BudgetStatus::Exceeded {
                    break 'outer;
                }

                selected.push(candidate.inner.id());
                continue 'outer;
            }

            break 'outer;
        }

        // Assemble batch and update internal state.
        let mut block_node = BlockNode(vec![]);
        for batch in selected {
            // SAFETY: Selected txs come from the transaction pool and are unique.
            let batch = self.nodes.proven_batches.remove(&batch).unwrap();
            self.state.remove(&batch);
            block_node.0.push(batch);
        }

        let block_number = self.chain_tip.child();
        let block = block_node.0.iter().map(|batch| Arc::clone(&batch.inner)).collect();

        self.state.insert(NodeId::Block(block_number), &block_node);
        self.nodes.proposed_block = Some((block_number, block_node));

        self.inject_telemetry();
        (block_number, block)
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
    pub fn commit_block(&mut self, to_commit: BlockNumber) {
        let block = self
            .nodes
            .proposed_block
            .take_if(|(proposed, _)| proposed == &to_commit)
            .expect("block must be in progress to commit");

        self.nodes.committed_blocks.push_back(block);
        self.chain_tip = self.chain_tip.child();

        if self.nodes.committed_blocks.len() > self.config.state_retention.get() {
            let (_number, node) = self.nodes.committed_blocks.pop_front().unwrap();
            self.state.remove(&node);
        }
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
        // Only revert if the given block is actually inflight.
        //
        // This guards against extreme circumstances where multiple block proofs may be inflight at
        // once. Due to the distributed nature of the node, one can imagine a scenario where
        // multiple provers get the same job for example.
        //
        // FIXME: We should consider a more robust check here to identify the block by a hash.
        //        If multiple jobs are possible, then so are multiple variants with the same block
        //        number.
        if self
            .nodes
            .proposed_block
            .as_ref()
            .is_none_or(|(proposed, _)| proposed != &block)
        {
            return;
        }
        todo!();
        // self.inject_telemetry();
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

    fn authentication_staleness_check(
        &self,
        authentication_height: BlockNumber,
    ) -> Result<(), AddTransactionError> {
        let oldest = self.nodes.oldest_committed_block().unwrap_or_default();

        if authentication_height < oldest {
            return Err(AddTransactionError::StaleInputs {
                input_block: authentication_height,
                stale_limit: oldest,
            });
        }

        Ok(())
    }

    fn expiration_check(&self, expired_at: BlockNumber) -> Result<(), AddTransactionError> {
        let limit = self.chain_tip + self.config.expiration_slack;
        if expired_at <= limit {
            return Err(AddTransactionError::Expired { expired_at, limit });
        }

        Ok(())
    }
}
