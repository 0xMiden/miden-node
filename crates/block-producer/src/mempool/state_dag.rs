#![allow(dead_code, unused_variables, reason = "wip")]

use std::collections::{HashMap, HashSet};

use miden_objects::Word;
use miden_objects::account::AccountId;
use miden_objects::batch::BatchId;
use miden_objects::block::BlockNumber;
use miden_objects::note::{NoteId, Nullifier};
use miden_objects::transaction::TransactionId;

mod account;

// NODE TRAIT
// ================================================================================================

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum NodeId {
    Transaction(TransactionId),
    Batch(BatchId),
    Block(BlockNumber),
}

pub trait Node {
    fn id(&self) -> NodeId;

    fn account_transitions(&self) -> impl Iterator<Item = (AccountId, Word, Word)>;

    fn nullifiers(&self) -> impl Iterator<Item = Nullifier>;

    fn unauthenticated_notes(&self) -> impl Iterator<Item = NoteId>;

    fn output_notes(&self) -> impl Iterator<Item = NoteId>;
}

// GRAPH ERRORS
// ================================================================================================

type GraphResult<T> = Result<T, GraphError>;

#[derive(thiserror::Error, Debug)]
pub enum GraphError {}

// STATE DAG
// ================================================================================================

pub struct StateGraph {
    accounts: HashMap<AccountId, account::AccountState>,
    nullifiers: HashSet<Nullifier>,
}

impl StateGraph {
    /// Marks all state created by the [`Node`] as committed. The node will no longer be considered
    /// as either parent nor child in the depedency graph.
    ///
    /// Data is retained until it is [pruned](Self::prune) to provide overlap with the committed
    /// state in the store.
    ///
    /// # Errors
    ///
    /// Returns an error if any of the expected data is not associated with the [`Node`].
    pub fn commit(&mut self, node: &impl Node) -> GraphResult<()> {
        // We preserve atomicity by cloning the data we want to mutate and replacing it once it all
        // succeeds. This may seem wasteful, but its easy and may even be cheaper than doing a full
        // accounting.

        // TODO: do the other things as well.
        let mut updated_accounts = HashMap::new();
        for (account_id, from, to) in node.account_transitions() {
            let mut account = self.accounts.get(&account_id).cloned().expect("throw error");
            account.commit(node.id(), from, to)?;

            if updated_accounts.insert(account_id, account).is_some() {
                todo!("error");
            }
        }

        // Note: HashMap::extend does replace as intended.
        self.accounts.extend(updated_accounts);
        // Note: no need to touch nullifiers since those contain no dependency information. They
        // are therefore simply removed in pruning.

        Ok(())
    }

    /// Prunes the node's state from the graph.
    ///
    /// State which is actively in use (i.e. being depended on) is still retained. It is however
    /// marked as pruned and will be removed once it is no longer in use.
    pub fn prune(&mut self, node: &impl Node) -> GraphResult<()> {
        // We preserve atomicity by cloning the data we want to mutate and replacing it once it all
        // succeeds. This may seem wasteful, but its easy and may even be cheaper than doing a full
        // accounting.

        for nullifier in node.nullifiers() {
            if !self.nullifiers.contains(&nullifier) {
                todo!("throw error");
            }
        }

        let mut updated_accounts = HashMap::new();
        for (account_id, _from, to) in node.account_transitions() {
            let mut account = self.accounts.get(&account_id).cloned().expect("throw error");
            account.prune(to);

            // Only retain account's that are still in-use.
            if !account.is_unused() {
                if updated_accounts.insert(account_id, account).is_some() {
                    todo!("error");
                }
            }
        }

        // Note: HashMap::extend does replace as intended.
        self.accounts.extend(updated_accounts);
        for nullifier in node.nullifiers() {
            self.nullifiers.remove(&nullifier);
        }

        Ok(())
    }
}

/// Represents a committed part of state within the graph.
///
/// It distinguishes between recently committed state (`Committed::Recent`) and state whose block
/// has been pruned locally, but which cannot be removed yet as other inflight state depends on it.
#[derive(Clone)]
enum Committed<T> {
    /// State from a recently committed block.
    ///
    /// The mempool retains recently committed state to provide an overlap with the state in the
    /// store. This extends the time that a new transaction or batch have to fetch data from the
    /// store without racing against committed state being dropped from the mempool.
    Recent(T),
    /// State who's block is no longer retained locally in the mempool, but which is still required
    /// as a foundation for inflight state.
    Pruned(T),
}

impl<T> Committed<T> {
    fn inner(&self) -> &T {
        match self {
            Committed::Recent(inner) => inner,
            Committed::Pruned(inner) => inner,
        }
    }

    fn is_pruned(&self) -> bool {
        matches!(self, Committed::Pruned(_))
    }
}
