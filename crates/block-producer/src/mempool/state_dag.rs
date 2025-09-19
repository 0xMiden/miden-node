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
use miden_objects::note::{NoteId, Nullifier};
use miden_objects::transaction::{TransactionHeader, TransactionId};

use crate::domain::transaction::AuthenticatedTransaction;
use crate::errors::{AddTransactionError, VerifyTxError};
use crate::mempool::{BatchBudget, BlockBudget, BudgetStatus};

// STATE DAG
// ================================================================================================

/// Tracks the inflight state of the mempool and models the relationship of the transactions,
/// batches and blocks as a DAG.
///
/// This model allows it to propose batches and blocks from the available transactions and proven
/// batches respectively. New transactions are validated against the current inflight state.
#[derive(Clone, Debug, PartialEq)]
pub struct StateDag {
    /// The DAG nodes describing the transactions, batches and blocks currently in the mempool.
    ///
    /// Only active nodes are stored e.g. a proposed batch will _replace_ all its transaction
    /// nodes. Relationships between the nodes are inferred using the state which essentially
    /// describes the edges of the DAG.
    ///
    /// This is kept in-sync with the state property i.e. the state is an aggregation of the
    /// nodes' state.
    nodes: Nodes,

    /// Describes the committed, created and consumed state of the nodes in the graph. The creating
    /// and consuming [`NodeId's`] are stored alongside the state, which allows this to act as the
    /// edges of the DAG.
    state: InflightState,

    /// Number of committed block's whose state we retain.
    ///
    /// This provides an overlap with the committed state from the store, giving new data's
    /// authentication inputs a grace period before it is submitted to the mempool. Without this,
    /// fetching the inputs from the store would race against new block being committed.
    state_retention: NonZeroUsize,

    /// How far from the current chain tip submitted data may expire.
    ///
    /// This prevents us from dealing with data that expires before it can be committed in a
    /// reasonable time.
    expiration_slack: u32,
}

#[derive(Clone, Debug, PartialEq, Default)]
struct InflightState {
    chain_tip: BlockNumber,

    /// Contains _all_ nullifiers created by inflight transactions, batches and blocks, including
    /// the recently committed blocks.
    ///
    /// This _includes_ erased nullifiers to make state reversion and re-insertion easier to reason
    /// about.
    nullifiers: HashSet<Nullifier>,

    /// Contains _all_ notes created by inflight transactions, batches and blocks, including
    /// the recently committed blocks.
    ///
    /// This _includes_ erased notes to make state reversion and re-insertion easier to reason
    /// about.
    ///
    /// This is tracked separately from note creation and consumption as those may exclude erased
    /// notes.
    output_notes: HashSet<NoteId>,
    // TODO: track account state
    // TODO: track notes
}

impl InflightState {
    fn nullifier_check(
        &self,
        nullifiers: impl Iterator<Item = Nullifier>,
    ) -> Result<(), AddTransactionError> {
        let exists = nullifiers
            .filter(|nullifier| self.nullifiers.contains(nullifier))
            .collect::<Vec<_>>();

        if exists.is_empty() {
            Ok(())
        } else {
            Err(VerifyTxError::InputNotesAlreadyConsumed(exists).into())
        }
    }

    fn output_notes_check(
        &self,
        notes: impl Iterator<Item = NoteId>,
    ) -> Result<(), AddTransactionError> {
        let exists = notes.filter(|note| self.output_notes.contains(note)).collect::<Vec<_>>();

        if exists.is_empty() {
            Ok(())
        } else {
            Err(VerifyTxError::OutputNotesAlreadyExist(exists).into())
        }
    }
}

// DAG NODES
// ================================================================================================

/// Uniquely identifies a node in the DAG.
///
/// This effectively describes the lifecycle of a transaction in the mempool.
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

impl ProvenBatchNode {
    fn tx_headers(&self) -> impl Iterator<Item = &TransactionHeader> {
        self.inner.transactions().as_slice().iter()
    }
}

#[derive(Clone, Debug, PartialEq)]
struct BlockNode(Vec<ProvenBatchNode>);

/// Describes a DAG node's impact on the state.
///
/// This is used to determine what state data is created or consumed by this node.
trait Node {
    fn nullifiers(&self) -> Box<dyn Iterator<Item = Nullifier> + '_>;
    fn all_output_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_>;
}

impl Node for TransactionNode {
    fn nullifiers(&self) -> Box<dyn Iterator<Item = Nullifier> + '_> {
        Box::new(self.0.nullifiers())
    }

    fn all_output_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_> {
        Box::new(self.0.output_note_ids())
    }
}

impl Node for ProposedBatchNode {
    fn nullifiers(&self) -> Box<dyn Iterator<Item = Nullifier> + '_> {
        Box::new(self.0.iter().flat_map(Node::nullifiers))
    }

    fn all_output_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_> {
        Box::new(self.0.iter().flat_map(Node::all_output_notes))
    }
}

impl Node for ProvenBatchNode {
    fn nullifiers(&self) -> Box<dyn Iterator<Item = Nullifier> + '_> {
        Box::new(self.tx_headers().flat_map(|tx| tx.input_notes().iter().copied()))
    }

    fn all_output_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_> {
        Box::new(self.tx_headers().flat_map(|tx| tx.output_notes().iter().copied()))
    }
}

impl Node for BlockNode {
    fn nullifiers(&self) -> Box<dyn Iterator<Item = Nullifier> + '_> {
        Box::new(self.0.iter().flat_map(Node::nullifiers))
    }

    fn all_output_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_> {
        Box::new(self.0.iter().flat_map(Node::all_output_notes))
    }
}

/// Contains the current nodes of the state DAG.
///
/// Nodes are purposefully not stored as a single collection since we often want to iterate through
/// specific node types e.g. all available transactions.
///
/// This data _must_ be kept in sync with the [`InflightState's`] [`NodeIds`] since these are used
/// as the edges of the graph.
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

impl Nodes {
    fn get(&self, id: &NodeId) -> Option<&dyn Node> {
        match id {
            NodeId::Transaction(id) => self.txs.get(id).map(|x| x as &dyn Node),
            NodeId::ProposedBatch(id) => self.proposed_batches.get(id).map(|x| x as &dyn Node),
            NodeId::ProvenBatch(id) => self.proven_batches.get(id).map(|x| x as &dyn Node),
            NodeId::ProposedBlock(id) => self
                .proposed_block
                .as_ref()
                .filter(|(number, _)| number == id)
                .map(|(_, x)| x as &dyn Node),
            NodeId::CommittedBlock(id) => self
                .committed_blocks
                .iter()
                .find(|(number, _)| number == id)
                .map(|(_, x)| x as &dyn Node),
        }
    }
}

// STATE DAG PUBLIC API
// ================================================================================================

impl StateDag {
    /// Creates an empty [`StateDag`] with the given configuration.
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

    /// Appends the transaction to the state graph if possible.
    pub fn append_transaction(
        &mut self,
        tx: Arc<AuthenticatedTransaction>,
    ) -> Result<(), AddTransactionError> {
        self.authentication_staleness_check(tx.authentication_height())?;
        self.expiration_check(tx.expires_at())?;

        let tx_id = tx.id();
        let node_id = NodeId::Transaction(tx.id());
        let node = TransactionNode(tx);

        self.state.nullifier_check(node.nullifiers())?;
        self.state.output_notes_check(node.all_output_notes())?;
        // TODO: unauthenticated note check
        // TODO: account state check

        self.nodes.txs.insert(tx_id, node);
        self.insert_into_state_unchecked(node_id);

        Ok(())
    }

    /// Selects a set of transactions for inclusion in a batch within the constraints of the budget.
    ///
    /// Returns [`None`] if no transactions are available for selection.
    pub fn propose_batch(
        &mut self,
        mut budget: BatchBudget,
    ) -> Option<(BatchId, Vec<Arc<AuthenticatedTransaction>>)> {
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

    /// Blindly inserts the node's state data into [`InflightState`] and associates it with the
    /// [`NodeId`].
    ///
    /// For state data that already exists, this is equivalent to overwriting the [`NodeId`]
    /// associated with the data.
    ///
    /// Callers _must_ ensure that the data is inline with the DAG expectations _and_ that the node
    /// data is already present in [`Nodes`].
    fn insert_into_state_unchecked(&mut self, id: NodeId) {
        // SAFETY: The node is expected to exist as part of this functions invariants.
        let node = self.nodes.get(&id).expect("node must exist in the DAG");

        self.state.nullifiers.extend(node.nullifiers());
        self.state.output_notes.extend(node.all_output_notes());
    }

    /// Returns all _uncommitted_ parent node's of the given [`NodeId`].
    ///
    /// # Panics
    ///
    /// Panics if the provided node is not present in the DAG.
    fn parents(&self, id: NodeId) -> HashSet<NodeId> {
        todo!();
    }

    /// Reverts the given node and **all** of its descendents.
    ///
    /// # Panics
    ///
    /// Panics if the provided node is not present in the DAG.
    fn revert_subtree_unchecked(&mut self, id: NodeId) {
        todo!();
    }

    /// Marks the block's associated state as pruned, removing the state if its no longer used.
    ///
    /// [`InflightState`] retains pruned data until the consumer of it is either committed or
    /// reverted.
    fn prune_unchecked(&mut self, number: BlockNumber, node: BlockNode) {
        for nullifier in node.nullifiers() {
            self.state.nullifiers.remove(&nullifier);
        }

        for note in node.all_output_notes() {
            self.state.output_notes.remove(&note);
        }
    }
}
