#![allow(
    dead_code,
    clippy::unused_self,
    unused_variables,
    clippy::needless_pass_by_value,
    reason = "wip"
)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::num::NonZeroUsize;
use std::sync::Arc;

use miden_objects::batch::{BatchId, ProvenBatch};
use miden_objects::block::BlockNumber;
use miden_objects::note::Nullifier;
use miden_objects::transaction::TransactionId;

use crate::domain::transaction::AuthenticatedTransaction;
use crate::errors::AddTransactionError;
use crate::mempool::{BatchBudget, BlockBudget, BudgetStatus};

// STATE DAG
// ================================================================================================

#[derive(Clone, Debug, PartialEq)]
pub struct StateDag {
    nodes: Nodes,
    state: InflightState,

    // Configuration
    state_retention: NonZeroUsize,
    expiration_slack: u32,
}

#[derive(Clone, Debug, PartialEq, Default)]
struct InflightState {
    chain_tip: BlockNumber,
    nullifiers: HashSet<Nullifier>,
}

// DAG NODES
// ================================================================================================

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
enum NodeId {
    Transaction(TransactionId),
    // UserBatch(BatchId),
    ProposedBatch(BatchId),
    ProvenBatch(BatchId),
    ProposedBlock(BlockNumber),
    CommittedBlock(BlockNumber),
}

#[derive(Clone, Debug, PartialEq)]
struct TransactionNode(Arc<AuthenticatedTransaction>);

#[derive(Clone, Debug, PartialEq)]
struct ProposedBatchNode(Vec<TransactionNode>);

#[derive(Clone, Debug, PartialEq)]
struct ProvenBatchNode {
    txs: Vec<TransactionNode>,
    inner: Arc<ProvenBatch>,
}

#[derive(Clone, Debug, PartialEq)]
struct BlockNode(Vec<ProvenBatchNode>);

trait Node {}

impl Node for TransactionNode {}
impl Node for ProposedBatchNode {}
impl Node for ProvenBatchNode {}
impl Node for BlockNode {}

#[derive(Clone, Debug, PartialEq, Default)]
struct Nodes {
    // Nodes in the DAG
    txs: HashMap<TransactionId, TransactionNode>,
    // user_batches: HashMap<BatchId, ProvenBatchNode>,
    proposed_batches: HashMap<BatchId, ProposedBatchNode>,
    proven_batches: HashMap<BatchId, ProvenBatchNode>,
    proposed_block: Option<(BlockNumber, BlockNode)>,
    committed_blocks: VecDeque<(BlockNumber, BlockNode)>,
}

// STATE DAG PUBLIC API
// ================================================================================================

impl StateDag {
    pub fn new(
        chain_tip: BlockNumber,
        state_retention: NonZeroUsize,
        expiration_slack: u32,
    ) -> Self {
        Self {
            state_retention,
            expiration_slack,
            nodes: Nodes::default(),
            state: InflightState { chain_tip, ..Default::default() },
        }
    }

    pub fn append_transaction(
        &mut self,
        tx: Arc<AuthenticatedTransaction>,
    ) -> Result<(), AddTransactionError> {
        self.authentication_staleness_check(tx.authentication_height())?;
        self.expiration_check(tx.expires_at())?;

        let tx_id = tx.id();
        let node_id = NodeId::Transaction(tx.id());
        let node = TransactionNode(tx);

        // TODO: check state is available

        self.nodes.txs.insert(tx_id, node);
        self.insert_into_state_unchecked(node_id);

        Ok(())
    }

    pub fn propose_batch(
        &mut self,
        mut budget: BatchBudget,
    ) -> Option<(BatchId, Vec<Arc<AuthenticatedTransaction>>)> {
        let mut selected = Vec::new();

        'outer: loop {
            'next_candidate: for candidate in self.nodes.txs.values().map(|tx| &tx.0) {
                if selected.contains(&candidate.id()) {
                    continue;
                }

                let parents = self.parents(NodeId::Transaction(candidate.id()));
                for parent in parents {
                    match parent {
                        NodeId::Transaction(parent) if !selected.contains(&parent) => {
                            continue 'next_candidate;
                        },
                        _ => {},
                    }
                }

                if budget.check_then_subtract(candidate.as_ref()) == BudgetStatus::Exceeded {
                    break 'outer;
                }

                selected.push(candidate.id());
                continue 'outer;
            }

            break 'outer;
        }

        if selected.is_empty() {
            return None;
        }

        // Assemble batch and update internal state.
        let mut tx_nodes = Vec::with_capacity(selected.len());
        for tx in selected {
            // SAFETY: Selected txs come from the transaction pool and are unique.
            tx_nodes.push(self.nodes.txs.remove(&tx).unwrap());
        }

        let batch_id =
            BatchId::from_transactions(tx_nodes.iter().map(|tx| tx.0.raw_proven_transaction()));
        let node = ProposedBatchNode(tx_nodes);
        let batch = node.0.iter().map(|tx| Arc::clone(&tx.0)).collect();

        self.nodes.proposed_batches.insert(batch_id, node);
        // FIXME: missing safety
        self.insert_into_state_unchecked(NodeId::ProposedBatch(batch_id));

        Some((batch_id, batch))
    }

    pub fn revert_proposed_batch(&mut self, batch: BatchId) {
        // Due to the distributed nature of the system, its possible that a proposed batch was
        // already proven, or already reverted. This guards against this eventuality.
        if !self.nodes.proposed_batches.contains_key(&batch) {
            return;
        }

        // FIXME: missing safety
        self.revert_subtree_unchecked(NodeId::ProposedBatch(batch));
    }

    pub fn submit_batch_proof(&mut self, proof: Arc<ProvenBatch>) {
        // Due to the distributed nature of the system, its possible that a proposed batch was
        // already proven, or already reverted. This guards against this eventuality.
        let Some(ProposedBatchNode(txs)) = self.nodes.proposed_batches.remove(&proof.id()) else {
            return;
        };

        // Propagate the node ID change internally.
        let batch_id = proof.id();
        let node = ProvenBatchNode { txs, inner: proof };
        self.nodes.proven_batches.insert(batch_id, node);

        // FIXME: missing safety
        self.insert_into_state_unchecked(NodeId::ProvenBatch(batch_id));
    }

    pub fn propose_block(
        &mut self,
        mut budget: BlockBudget,
    ) -> (BlockNumber, Vec<Arc<ProvenBatch>>) {
        assert!(
            self.nodes.proposed_block.is_none(),
            "block {} is already in progress",
            self.nodes.proposed_block.as_ref().unwrap().0
        );

        let mut selected = Vec::new();

        'outer: loop {
            'next_candidate: for candidate in
                self.nodes.proven_batches.values().map(|node| &node.inner)
            {
                if selected.contains(&candidate.id()) {
                    continue;
                }

                let parents = self.parents(NodeId::ProvenBatch(candidate.id()));
                for parent in parents {
                    match parent {
                        NodeId::ProposedBlock(block) => panic!(
                            "Block selection encountered an existing proposed block ID {block} while navigating the state DAG"
                        ),
                        NodeId::ProvenBatch(batch) if selected.contains(&batch) => {},
                        NodeId::CommittedBlock(_) => {},
                        _ => continue 'next_candidate,
                    }
                }

                if budget.check_then_subtract(candidate.as_ref()) == BudgetStatus::Exceeded {
                    break 'outer;
                }

                selected.push(candidate.id());
                continue 'outer;
            }

            break 'outer;
        }

        // Assemble batch and update internal state.
        let mut batch_nodes = Vec::with_capacity(selected.len());
        for batch in selected {
            // SAFETY: Selected txs come from the transaction pool and are unique.
            batch_nodes.push(self.nodes.proven_batches.remove(&batch).unwrap());
        }

        let block_number = self.state.chain_tip.child();
        let node = BlockNode(batch_nodes);
        let block = node.0.iter().map(|batch| Arc::clone(&batch.inner)).collect();

        self.nodes.proposed_block = Some((block_number, node));
        // FIXME: missing safety
        self.insert_into_state_unchecked(NodeId::ProposedBlock(block_number));

        (block_number, block)
    }

    pub fn revert_proposed_block(&mut self, block: BlockNumber) {
        // Only revert if the given block is actually inflight.
        //
        // This guards against extreme circumstances where its possible that multiple block proofs
        // may be inflight at once. Due to the distributed nature of the node, one can imagine a
        // scenario where multiple provers get the same job.
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

        // FIXME: missing safety
        self.revert_subtree_unchecked(NodeId::ProposedBlock(block));
    }

    pub fn commit_proposed_block(&mut self, to_commit: BlockNumber) {
        let block = self
            .nodes
            .proposed_block
            .take_if(|(proposed, _)| proposed == &to_commit)
            .expect("block must be in progress to commit");

        self.nodes.committed_blocks.push_back(block);
        // FIXME: Missing safety
        self.insert_into_state_unchecked(NodeId::CommittedBlock(to_commit));
        self.state.chain_tip = to_commit;

        if self.nodes.committed_blocks.len() > self.state_retention.get() {
            let (number, node) = self.nodes.committed_blocks.pop_front().unwrap();
            self.prune_unchecked(number, node);
        }
    }

    pub fn chain_tip(&self) -> BlockNumber {
        self.state.chain_tip
    }
}

// STATE DAG INTERNAL FUNCTIONALITY
// ================================================================================================

impl StateDag {
    fn authentication_staleness_check(
        &self,
        authentication_height: BlockNumber,
    ) -> Result<(), AddTransactionError> {
        let oldest = self
            .nodes
            .committed_blocks
            .front()
            .map(|(oldest, _)| *oldest)
            .unwrap_or_default();

        if authentication_height < oldest {
            return Err(AddTransactionError::StaleInputs {
                input_block: authentication_height,
                stale_limit: oldest,
            });
        }

        Ok(())
    }

    fn expiration_check(&self, expired_at: BlockNumber) -> Result<(), AddTransactionError> {
        let limit = self.state.chain_tip + self.expiration_slack;
        if expired_at <= limit {
            return Err(AddTransactionError::Expired { expired_at, limit });
        }

        Ok(())
    }

    fn insert_into_state_unchecked(&mut self, id: NodeId) {}

    fn parents(&self, id: NodeId) -> HashSet<NodeId> {
        todo!();
    }

    fn revert_subtree_unchecked(&mut self, id: NodeId) {}

    fn prune_unchecked(&mut self, number: BlockNumber, node: BlockNode) {
        todo!();
    }
}
