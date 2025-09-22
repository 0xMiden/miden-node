//! The [`Mempool`] is responsible for receiving transactions, and proposing transactions for
//! inclusion in batches, and proposing batches for inclusion in the next block.
//!
//! It performs this task by maintaining a directed acyclic graph (DAG). Nodes in this graph
//! represent transactions, batches and blocks. The DAG nature of the graph is important as this
//! allows us to propose batches and blocks such that they only depend on already proposed nodes.
//!
//! This requirement allows us to create sensible nodes such that they are not co-dependent aka have
//! no cycles. A cycle between batches would require that all batches within that cycle _must_ all
//! be committed as part of the same block. This follows from the requirement that the next block
//! in the blockchain _must_ be a transition from the current latest block's state.
//!
//! While this is technically possible, the bookkeeping and implementation to allow this are
//! infeasible, and both blocks and batches have constraints. This is also undersireable since if
//! one component of such a cycle fails or expires, then all others would likewise need to be
//! reverted.
//!
//! The DAG nature of the graph is maintained by:
//!
//! - Ensuring incoming transactions are only ever appended to the current graph. This in turn
//!   implies that the transaction's state transition must build on top of the current mempool
//!   state.
//! - Parent/child edges between nodes in the graph are formed via state dependency.
//! - Transactions are proposed for batch inclusion only once _all_ its ancestors have already been
//!   included in a batch.
//! - Similarly, batches are proposed for block inclusion once _all_ ancestors have been included in
//!   a block.
//! - Reverting a node reverts all descendents as well.

use std::collections::HashSet;
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
use crate::mempool::nodes::{BlockNode, Node, NodeId, ProposedBatchNode, TransactionNode};
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
        let tx = TransactionNode::new(tx);
        self.state.insert(NodeId::Transaction(tx.id()), &tx);
        self.nodes.txs.insert(tx.id(), tx);

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
        // Note that selecting a transaction can unblock other transactions. This implementation
        // handles this by resetting the iteration whenever a transaction is selected.
        //
        // This is still reasonably performant given that we only retain unselected transactions as
        // transaction nodes i.e. selected transactions become batch nodes.
        //
        // The additional bookkeeping can be implemented once we have fee related strategies. KISS.

        let mut selected = ProposedBatchNode::default();
        let mut budget = self.config.batch_budget;

        let mut candidates = self.nodes.txs.values();

        'next: while let Some(candidate) = candidates.next() {
            if selected.contains(candidate.id()) {
                continue 'next;
            }

            // A transaction may be placed in a batch IFF all parents were already in a batch (or
            // are part of this one).
            for parent in self.state.parents(candidate) {
                match parent {
                    // TODO(mirko): Once user batches are supported, they will also need to be
                    // checked here.
                    NodeId::Transaction(parent) if !selected.contains(parent) => continue 'next,
                    NodeId::Transaction(_)
                    | NodeId::ProposedBatch(_)
                    | NodeId::ProvenBatch(_)
                    | NodeId::Block(_) => {},
                }
            }

            if budget.check_then_subtract(&candidate.inner()) == BudgetStatus::Exceeded {
                break;
            }

            candidates = self.nodes.txs.values();
            selected.push(candidate.inner().clone());
        }

        if selected.is_empty() {
            return None;
        }

        let batch_id = selected.calculate_id();
        let batch_txs = selected.transactions().to_vec();

        for tx in &batch_txs {
            let node =
                self.nodes.txs.remove(&tx.id()).expect("selected transaction node must exist");
            self.state.remove(&node);
        }
        self.state.insert(NodeId::ProposedBatch(batch_id), &selected);
        self.nodes.proposed_batches.insert(batch_id, selected);

        // TODO(mirko): Selecting a batch can unblock user batches, which should be checked here.

        self.inject_telemetry();
        Some((batch_id, batch_txs))
    }

    /// Drops the failed batch and all of its descendants.
    ///
    /// Transactions are re-queued.
    #[instrument(target = COMPONENT, name = "mempool.rollback_batch", skip_all)]
    pub fn rollback_batch(&mut self, batch: BatchId) {
        // Due to the distributed nature of the system, its possible that a proposed batch was
        // already proven, or already reverted. This guards against this eventuality.
        if !self.nodes.proposed_batches.contains_key(&batch) {
            return;
        };

        // TODO(mirko): This will be somewhat complicated by the introduction of user batches
        // since these don't have all inner txs present and therefore must at least partially
        // revert their own tx descendents.

        // Remove all descendent batches and reinsert their transactions. This is safe to do since
        // a batch's state impact is the aggregation of its transactions.
        let mut processed = HashSet::<NodeId>::default();
        let mut to_process = Vec::default();
        to_process.push(NodeId::ProposedBatch(batch));

        while let Some(id) = to_process.pop() {
            if processed.contains(&id) {
                continue;
            }

            let to_reinsert = match id {
                // We want to re-queue transactions, so we may as well leave them in place.
                NodeId::Transaction(_) => continue,
                NodeId::ProposedBatch(batch_id) => {
                    // SAFETY: all IDs originate from within the graph and therefore must be valid.
                    let node = self.nodes.proposed_batches.remove(&batch_id).unwrap();
                    to_process.extend(self.state.children(&node));
                    self.state.remove(&node);
                    node.transactions().to_vec()
                },
                NodeId::ProvenBatch(batch_id) => {
                    // SAFETY: all IDs originate from within the graph and therefore must be valid.
                    let node = self.nodes.proven_batches.remove(&batch_id).unwrap();
                    to_process.extend(self.state.children(&node));
                    self.state.remove(&node);
                    node.transactions().to_vec()
                },
                NodeId::Block(block) => {
                    // A block requries that all ancestors be blocks, and therefore this should not
                    // be possible. See block selection for more info.
                    panic!("found block {block} descendent while rolling back batch {batch}")
                },
            };

            for tx in to_reinsert {
                let node = TransactionNode::new(tx);
                self.state.insert(NodeId::Transaction(node.id()), &node);
                self.nodes.txs.insert(node.id(), node);
            }

            processed.insert(id);
        }

        self.inject_telemetry();
    }

    /// Marks a batch as proven if it exists.
    #[instrument(target = COMPONENT, name = "mempool.commit_batch", skip_all)]
    pub fn commit_batch(&mut self, proof: Arc<ProvenBatch>) {
        // Due to the distributed nature of the system, its possible that a proposed batch was
        // already proven, or already reverted. This guards against this eventuality.
        let Some(proposed) = self.nodes.proposed_batches.remove(&proof.id()) else {
            return;
        };

        self.state.remove(&proposed);

        let proven = proposed.with_proof(proof);
        self.state.insert(NodeId::ProvenBatch(proven.id()), &proven);
        self.nodes.proven_batches.insert(proven.id(), proven);

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
        // The selection algorithm is fairly neanderthal in nature.
        //
        // We iterate over all proven batch nodes, each time selecting the first which has no
        // parent nodes that are unselected batches.
        //
        // Note that selecting a batch can unblock other batches. This implementation handles this
        // by resetting the iteration whenever a batch is selected.

        assert!(
            self.nodes.proposed_block.is_none(),
            "block {} is already in progress",
            self.nodes.proposed_block.as_ref().unwrap().0
        );

        let mut selected = BlockNode::default();
        let mut budget = self.config.block_budget;
        let mut candidates = self.nodes.proven_batches.values();

        'next: while let Some(candidate) = candidates.next() {
            if selected.contains(candidate.id()) {
                continue 'next;
            }

            // A batch is selectable if all parents are already blocks, or if the batch is part of
            // the current block selection.
            for parent in self.state.parents(candidate) {
                match parent {
                    NodeId::Block(_) => {},
                    NodeId::ProvenBatch(parent) if !selected.contains(parent) => {},
                    _ => continue 'next,
                }
            }

            if budget.check_then_subtract(candidate.inner()) == BudgetStatus::Exceeded {
                break;
            }

            // Reset iteration as this batch could have unblocked previous batches.
            candidates = self.nodes.proven_batches.values();
            selected.push(candidate.clone());
        }

        // Replace the batches with the block in state and nodes.
        for batch in selected.batches() {
            // SAFETY: Selected batches came from nodes, and are unique.
            let batch = self.nodes.proven_batches.remove(&batch.id()).unwrap();
            self.state.remove(&batch);
        }

        let block_number = self.chain_tip.child();
        let batches = selected.batches().to_vec();

        self.state.insert(NodeId::Block(block_number), &selected);
        self.nodes.proposed_block = Some((block_number, selected));

        self.inject_telemetry();
        (block_number, batches)
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
