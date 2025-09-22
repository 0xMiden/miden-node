use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use miden_objects::Word;
use miden_objects::account::AccountId;
use miden_objects::note::{NoteId, Nullifier};

use crate::mempool::nodes::{Node, NodeId};

/// Tracks the inflight state of the mempool and the [`NodeId`]s associated with each piece of it.
///
/// This allows it to track the dependency relationships between nodes in the mempool's state DAG by
/// checking which [`NodeId`]s created or consumed the data the node relies on.
///
/// Note that the user is responsible for ensuring that the inserted nodes adhere to a DAG
/// structure. No attempt is made to verify this internally here as it requires more information
/// than is available at this level.
#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) struct InflightState {
    nullifiers: HashSet<Nullifier>,
    output_notes: HashMap<NoteId, NodeId>,
    authenticated_notes: HashMap<NoteId, NodeId>,
    accounts: HashMap<AccountId, AccountUpdates>,
}

impl InflightState {
    /// Returns all nullifiers which already exist.
    pub(crate) fn nullifiers_exist(
        &self,
        nullifiers: impl Iterator<Item = Nullifier>,
    ) -> Vec<Nullifier> {
        nullifiers.filter(|nullifier| self.nullifiers.contains(nullifier)).collect()
    }

    /// Returns all output notes which already exist.
    pub(crate) fn output_notes_exist(&self, notes: impl Iterator<Item = NoteId>) -> Vec<NoteId> {
        notes.filter(|note| self.output_notes.contains_key(note)).collect()
    }

    /// The latest account commitment tracked by the inflight state.
    ///
    /// A [`None`] value _does not_ mean this account doesn't exist at all, but rather that it
    /// has no inflight nodes.
    pub(crate) fn account_commitment(&self, account: &AccountId) -> Option<Word> {
        self.accounts.get(account).map(AccountUpdates::latest_commitment)
    }

    pub(crate) fn remove(&mut self, node: &dyn Node) {
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
                // TODO: consider panicking since this shouldn't happen? But we don't assert
                // about either..
                continue;
            };

            if entry.get().is_empty() {
                entry.remove_entry();
            }
        }
    }

    pub(crate) fn insert(&mut self, id: NodeId, node: &dyn Node) {
        self.nullifiers.extend(node.nullifiers());
        self.output_notes.extend(node.output_notes().map(|note| (note, id)));
        self.authenticated_notes
            .extend(node.unauthenticated_notes().map(|note| (note, id)));

        for (account, from, to) in node.account_updates() {
            self.accounts.entry(account).or_default().insert(id, from, to);
        }
    }

    /// The [`NodeId`]s which the given node directly depends on.
    ///
    /// Note that the result is invalidated by mutating the state.
    pub(crate) fn parents(&self, node: &dyn Node) -> HashSet<NodeId> {
        let note_parents =
            node.unauthenticated_notes().filter_map(|note| self.output_notes.get(&note));

        let account_parents = node
            .account_updates()
            .filter_map(|(account, from, _to)| {
                self.accounts.get(&account).map(|account| account.parent(&from))
            })
            .flatten();

        account_parents.chain(note_parents).copied().collect()
    }

    /// The [`NodeId`]s which depend directly on the given node.
    ///
    /// Note that the result is invalidated by mutating the state.
    pub(crate) fn children(&self, node: &dyn Node) -> HashSet<NodeId> {
        let note_children =
            node.output_notes().filter_map(|note| self.authenticated_notes.get(&note));

        let account_children = node
            .account_updates()
            .filter_map(|(account, _from, to)| {
                self.accounts.get(&account).map(|account| account.child(&to))
            })
            .flatten();

        account_children.chain(note_children).copied().collect()
    }
}

/// The commitment updates made to a account.
#[derive(Clone, Debug, PartialEq, Default)]
struct AccountUpdates {
    from: HashMap<Word, NodeId>,
    to: HashMap<Word, NodeId>,
}

impl AccountUpdates {
    fn latest_commitment(&self) -> Word {
        // The latest commitment will be whichever commitment isn't consumed aka a `to` which has
        // no `from`. This breaks if this isn't in a valid state.
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

    fn parent(&self, from: &Word) -> Option<&NodeId> {
        self.to.get(from)
    }

    fn child(&self, to: &Word) -> Option<&NodeId> {
        self.from.get(to)
    }
}
