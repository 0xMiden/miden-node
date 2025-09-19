#![allow(
    dead_code,
    clippy::unused_self,
    unused_variables,
    clippy::needless_pass_by_value,
    reason = "wip"
)]

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::num::NonZeroUsize;
use std::sync::Arc;

use miden_objects::Word;
use miden_objects::account::AccountId;
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
    nullifiers: HashSet<Nullifier>,
    output_notes: HashMap<NoteId, NodeId>,
    authenticated_notes: HashMap<NoteId, NodeId>,
    accounts: HashMap<AccountId, AccountUpdates>,
}

#[derive(Clone, Debug, PartialEq, Default)]
struct AccountUpdates {
    from: HashMap<Word, NodeId>,
    to: HashMap<Word, NodeId>,
}

impl AccountUpdates {
    fn latest_commitment(&self) -> Word {
        self.to
            .keys()
            .find(|commitment| !self.from.contains_key(commitment))
            .copied()
            .unwrap_or_default()
    }

    fn is_empty(&self) -> bool {
        self.from.is_empty() && self.to.is_empty()
    }

    fn remove(&mut self, from: Word, to: Word) {
        self.from.remove(&from);
        self.to.remove(&to);
    }

    fn insert(&mut self, id: NodeId, from: Word, to: Word) {
        self.from.insert(from, id);
        self.to.insert(to, id);
    }
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
        let exists = notes.filter(|note| self.output_notes.contains_key(note)).collect::<Vec<_>>();

        if exists.is_empty() {
            Ok(())
        } else {
            Err(VerifyTxError::OutputNotesAlreadyExist(exists).into())
        }
    }

    /// The latest account commitment tracked by the inflight state.
    ///
    /// A [`None`] value _does not_ mean this account doesn't exist at all, but rather that it has
    /// no inflight nodes.
    fn account_commitment(&self, account: &AccountId) -> Option<Word> {
        self.accounts.get(account).map(AccountUpdates::latest_commitment)
    }

    fn remove(&mut self, node: &dyn Node) {
        for nullifier in node.nullifiers() {
            self.nullifiers.remove(&nullifier);
        }

        for note in node.output_notes() {
            self.output_notes.remove(&note);
        }

        for note in node.unauthenticated_notes() {
            self.authenticated_notes.remove(&note);
        }

        for (account, from, to) in node.account_updates() {
            let Entry::Occupied(entry) =
                self.accounts.entry(account).and_modify(|entry| entry.remove(from, to))
            else {
                // TODO: consider panicking since this shouldn't happen? But we don't assert about
                // either..
                continue;
            };

            if entry.get().is_empty() {
                entry.remove_entry();
            }
        }
    }

    fn insert(&mut self, id: NodeId, node: &dyn Node) {
        self.nullifiers.extend(node.nullifiers());
        self.output_notes.extend(node.output_notes().map(|note| (note, id)));
        self.authenticated_notes
            .extend(node.unauthenticated_notes().map(|note| (note, id)));

        for (account, from, to) in node.account_updates() {
            self.accounts.entry(account).or_default().insert(id, from, to);
        }
    }

    /// The [`NodeIds`] which the given node depends on.
    ///
    /// Note that the result is invalidated by mutating the state.
    fn parents(&self, node: &dyn Node) -> HashSet<NodeId> {
        let note_parents = node
            .unauthenticated_notes()
            .filter_map(|note| self.output_notes.get(&note))
            .copied();

        // TODO: chain with account parents

        note_parents.collect()
    }

    /// The [`NodeIds`] which depend on the given node.
    ///
    /// Note that the result is invalidated by mutating the state.
    fn children(&self, node: &dyn Node) -> HashSet<NodeId> {
        let note_children = node
            .output_notes()
            .filter_map(|note| self.authenticated_notes.get(&note))
            .copied();

        // TODO: chain with account children

        note_children.collect()
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
    Block(BlockNumber),
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
    fn output_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_>;
    fn unauthenticated_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_>;
    fn account_updates(&self) -> Box<dyn Iterator<Item = (AccountId, Word, Word)> + '_>;
}

impl Node for TransactionNode {
    fn nullifiers(&self) -> Box<dyn Iterator<Item = Nullifier> + '_> {
        Box::new(self.0.nullifiers())
    }

    fn output_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_> {
        Box::new(self.0.output_note_ids())
    }

    fn unauthenticated_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_> {
        Box::new(self.0.unauthenticated_notes())
    }

    fn account_updates(&self) -> Box<dyn Iterator<Item = (AccountId, Word, Word)> + '_> {
        let update = self.0.account_update();
        Box::new(std::iter::once((
            update.account_id(),
            update.initial_state_commitment(),
            update.final_state_commitment(),
        )))
    }
}

impl Node for ProposedBatchNode {
    fn nullifiers(&self) -> Box<dyn Iterator<Item = Nullifier> + '_> {
        Box::new(self.0.iter().flat_map(Node::nullifiers))
    }

    fn output_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_> {
        Box::new(self.0.iter().flat_map(Node::output_notes))
    }

    fn unauthenticated_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_> {
        Box::new(self.0.iter().flat_map(Node::unauthenticated_notes))
    }

    fn account_updates(&self) -> Box<dyn Iterator<Item = (AccountId, Word, Word)> + '_> {
        Box::new(self.0.iter().flat_map(Node::account_updates))
    }
}

impl Node for ProvenBatchNode {
    fn nullifiers(&self) -> Box<dyn Iterator<Item = Nullifier> + '_> {
        Box::new(self.tx_headers().flat_map(|tx| tx.input_notes().iter().copied()))
    }

    fn output_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_> {
        Box::new(self.tx_headers().flat_map(|tx| tx.output_notes().iter().copied()))
    }

    fn unauthenticated_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_> {
        Box::new(
            self.inner
                .input_notes()
                .iter()
                .filter_map(|note| note.header())
                .map(|note| note.id()),
        )
    }

    fn account_updates(&self) -> Box<dyn Iterator<Item = (AccountId, Word, Word)> + '_> {
        Box::new(self.inner.account_updates().iter().map(|(_, update)| {
            (
                update.account_id(),
                update.initial_state_commitment(),
                update.final_state_commitment(),
            )
        }))
    }
}

impl Node for BlockNode {
    fn nullifiers(&self) -> Box<dyn Iterator<Item = Nullifier> + '_> {
        Box::new(self.0.iter().flat_map(Node::nullifiers))
    }

    fn output_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_> {
        Box::new(self.0.iter().flat_map(Node::output_notes))
    }

    fn unauthenticated_notes(&self) -> Box<dyn Iterator<Item = NoteId> + '_> {
        Box::new(self.0.iter().flat_map(Node::unauthenticated_notes))
    }

    fn account_updates(&self) -> Box<dyn Iterator<Item = (AccountId, Word, Word)> + '_> {
        Box::new(self.0.iter().flat_map(Node::account_updates))
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
            NodeId::Block(id) => self
                .committed_blocks
                .iter()
                .chain(&self.proposed_block)
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
        self.state.nullifier_check(tx.nullifiers())?;
        self.state.output_notes_check(tx.output_note_ids())?;

        // Insert the transaction node.
        let tx_id = tx.id();
        let node_id = NodeId::Transaction(tx.id());
        let node = TransactionNode(tx);

        self.state.insert(node_id, &node);
        self.nodes.txs.insert(tx_id, node);

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
        let Some(proposed) = self.nodes.proposed_batches.remove(&proof.id()) else {
            return;
        };

        // Propagate the node ID change internally.
        let batch_id = proof.id();
        self.state.remove(&proposed);

        let node = ProvenBatchNode { txs: proposed.0, inner: proof };
        self.state.insert(NodeId::ProvenBatch(batch_id), &node);
        self.nodes.proven_batches.insert(batch_id, node);
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

        let block_number = self.state.chain_tip.child();
        let block = block_node.0.iter().map(|batch| Arc::clone(&batch.inner)).collect();

        self.state.insert(NodeId::Block(block_number), &block_node);
        self.nodes.proposed_block = Some((block_number, block_node));

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
        self.revert_subtree_unchecked(NodeId::Block(block));
    }

    pub fn commit_proposed_block(&mut self, to_commit: BlockNumber) {
        let block = self
            .nodes
            .proposed_block
            .take_if(|(proposed, _)| proposed == &to_commit)
            .expect("block must be in progress to commit");

        self.nodes.committed_blocks.push_back(block);
        self.state.chain_tip = to_commit;

        if self.nodes.committed_blocks.len() > self.state_retention.get() {
            let (number, node) = self.nodes.committed_blocks.pop_front().unwrap();
            self.state.remove(&node);
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

    /// Reverts the given node and **all** of its descendents.
    ///
    /// # Panics
    ///
    /// Panics if the provided node is not present in the DAG.
    fn revert_subtree_unchecked(&mut self, id: NodeId) {
        todo!();
    }
}
