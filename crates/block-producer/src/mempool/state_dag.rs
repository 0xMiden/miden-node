#![allow(dead_code, unused_variables, reason = "wip")]

use std::collections::{HashMap, HashSet};

use miden_objects::Word;
use miden_objects::account::AccountId;
use miden_objects::batch::{BatchId, ProvenBatch};
use miden_objects::block::{BlockNumber, ProvenBlock};
use miden_objects::note::{NoteId, Nullifier};
use miden_objects::transaction::TransactionId;

use crate::domain::transaction::AuthenticatedTransaction;
use crate::mempool::state_dag::account::AccountState;

mod account;

// NODE TRAIT
// ================================================================================================

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum NodeId {
    Transaction(TransactionId),
    Batch(BatchId),
    Block(BlockNumber),
}

impl From<TransactionId> for NodeId {
    fn from(value: TransactionId) -> Self {
        Self::Transaction(value)
    }
}

impl From<BatchId> for NodeId {
    fn from(value: BatchId) -> Self {
        Self::Batch(value)
    }
}

impl From<BlockNumber> for NodeId {
    fn from(value: BlockNumber) -> Self {
        Self::Block(value)
    }
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
struct Node {
    account_updates: Vec<(AccountId, Word, Word)>,
    nullifiers: Vec<Nullifier>,
    output_notes: Vec<NodeId>,
}

impl From<&AuthenticatedTransaction> for Node {
    fn from(value: &AuthenticatedTransaction) -> Self {
        todo!()
    }
}

impl Node {
    fn extend(&mut self, other: &Self) {
        // TODO: more
        self.nullifiers.extend(&other.nullifiers);
        self.output_notes.extend(&other.output_notes);
    }
}

// GRAPH ERRORS
// ================================================================================================

type GraphResult<T> = Result<T, GraphError>;

#[derive(thiserror::Error, Debug)]
pub enum GraphError {
    #[error("node {0:?} is not present in the graph")]
    UnknownNode(NodeId),
}

// STATE DAG
// ================================================================================================

#[derive(Clone, Default, Debug, PartialEq)]
pub struct StateGraph {
    accounts: HashMap<AccountId, AccountState>,
    nullifiers: HashSet<Nullifier>,

    nodes: HashMap<NodeId, Node>,
}

impl StateGraph {
    pub fn parents(&self, id: impl Into<NodeId>) -> GraphResult<HashSet<NodeId>> {
        let node = self.get_node(id)?;
        let parents = HashSet::default();

        // TODO: lookup parents via state.

        Ok(parents)
    }

    pub fn children(&self, id: impl Into<NodeId>) -> GraphResult<HashSet<NodeId>> {
        let node = self.get_node(id)?;
        let children = HashSet::default();

        // TODO: lookup children via state.

        Ok(children)
    }

    pub fn append_transaction(&mut self, tx: &AuthenticatedTransaction) -> GraphResult<()> {
        let node = Node::from(tx);
        let id = NodeId::from(tx.id());

        // TODO: perform state checks
        // TODO: update state

        self.nodes.insert(id, node);

        Ok(())
    }

    pub fn insert_batch(
        &mut self,
        batch_id: BatchId,
        txs: impl Iterator<Item = TransactionId>,
    ) -> GraphResult<()> {
        let id = NodeId::from(batch_id);

        let mut node = Node::default();
        for tx in txs {
            let tx = self.get_node(tx)?;
            node.extend(tx);
        }

        // TODO: perform state checks
        // TODO: remove existing node
        // TODO: update state information with new NodeID

        self.nodes.insert(id, node);

        Ok(())
    }

    pub fn commit_block(
        &mut self,
        block_number: BlockNumber,
        batches: impl Iterator<Item = BatchId>,
    ) -> GraphResult<()> {
        todo!();
    }

    pub fn prune_block(&mut self, block: BlockNumber) -> GraphResult<()> {
        todo!();
    }

    // Submit a batch..
    // Commit a block..
    // Prune a block..

    /// Returns the associated [`Node`] or [`GraphError::UnknownNode`] if it doesn't exist.
    fn get_node(&self, id: impl Into<NodeId>) -> GraphResult<&Node> {
        let id = id.into();
        self.nodes.get(&id).ok_or_else(|| GraphError::UnknownNode(id))
    }
}
