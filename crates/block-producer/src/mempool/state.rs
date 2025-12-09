use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use miden_objects::Word;
use miden_objects::account::AccountId;
use miden_objects::note::Nullifier;

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
pub(super) struct InflightState {
    /// All nullifiers created by inflight state.
    ///
    /// This _includes_ nullifiers from erased notes to simplify reverting nodes and requeuing
    /// their transactions. If this weren't the case, then its possible that a batch contains an
    /// erased note which can clash with another inflight transaction. If we were to revert this
    /// batch and requeue its transactions, then this would be illegal. This is possible to handle
    /// but would be painful ito bookkeeping.
    ///
    /// Instead we opt to include erased notes in the tracking here, which allows us to revert the
    /// batch and requeue its transactions without having to worry about note clashes.
    nullifiers: HashSet<Nullifier>,

    /// All created notes and the ID of the node that created it.
    ///
    /// This _includes_ erased notes, see `nullifiers` for more information.
    output_notes: HashMap<Word, NodeId>,

    /// Maps all unauthenticated notes to the node ID that consumed them.
    ///
    /// This can be combined with `output_notes` to infer a parent<->child relationship between
    /// nodes i.e. the parent node that created a note and the child node that consumed the note.
    ///
    /// It is the callers responsibility to ensure that all unauthenticated notes exist as output
    /// notes at the time when a new transaction or batch node is inserted.
    unauthenticated_notes: HashMap<Word, NodeId>,

    /// All inflight account commitment transitions.
    accounts: HashMap<AccountId, AccountUpdates>,
}

impl InflightState {
    /// Returns all nullifiers which already exist.
    pub(super) fn nullifiers_exist(
        &self,
        nullifiers: impl Iterator<Item = Nullifier>,
    ) -> Vec<Nullifier> {
        nullifiers.filter(|nullifier| self.nullifiers.contains(nullifier)).collect()
    }

    /// Returns all output note commitments which already exist.
    pub(super) fn output_notes_exist(&self, notes: impl Iterator<Item = Word>) -> Vec<Word> {
        notes
            .filter(|note_commitment| self.output_notes.contains_key(note_commitment))
            .collect()
    }

    /// Returns all output notes which don't exist.
    pub(super) fn output_notes_missing(
        &self,
        note_commitments: impl Iterator<Item = Word>,
    ) -> Vec<Word> {
        note_commitments.filter(|note| !self.output_notes.contains_key(note)).collect()
    }

    /// The latest account commitment tracked by the inflight state.
    ///
    /// A [`None`] value _does not_ mean this account doesn't exist at all, but rather that it
    /// has no inflight nodes.
    pub(super) fn account_commitment(&self, account: &AccountId) -> Option<Word> {
        self.accounts.get(account).map(AccountUpdates::latest_commitment)
    }

    /// Removes all the state of the node from tracking.
    ///
    /// Note that this simply removes the data and does not check that the data was associated with
    /// the [`Node`] at the time of removal. The caller is responsible for ensuring that the given
    /// node was still active in the state.
    pub(super) fn remove(&mut self, node: &dyn Node) {
        for nullifier in node.nullifiers() {
            assert!(
                self.nullifiers.remove(&nullifier),
                "Nullifier {nullifier} was not present for removal"
            );
        }

        for note in node.output_note_commitments() {
            assert!(
                self.output_notes.remove(&note).is_some(),
                "Output note {note} was not present for removal"
            );
        }

        for note in node.unauthenticated_note_commitments() {
            assert!(
                self.unauthenticated_notes.remove(&note).is_some(),
                "Unauthenticated note {note} was not present for removal"
            );
        }

        for (account, from, to) in node.account_updates() {
            let Entry::Occupied(entry) = self
                .accounts
                .entry(account)
                .and_modify(|entry| entry.remove(node.id(), from, to))
            else {
                panic!("Account {account} update ({from} -> {to}) was not present for removal");
            };

            if entry.get().is_empty() {
                entry.remove_entry();
            }
        }
    }

    /// Inserts the node into the state, associating the data with the node's ID. This powers the
    /// parent and child relationship lookups.
    pub(super) fn insert(&mut self, id: NodeId, node: &dyn Node) {
        self.nullifiers.extend(node.nullifiers());
        self.output_notes
            .extend(node.output_note_commitments().map(|note_commitment| (note_commitment, id)));
        self.unauthenticated_notes.extend(
            node.unauthenticated_note_commitments()
                .map(|note_commitment| (note_commitment, id)),
        );

        for (account, from, to) in node.account_updates() {
            self.accounts.entry(account).or_default().insert(id, from, to);
        }
    }

    /// The [`NodeId`]s which the given node directly depends on.
    ///
    /// Note that the result is invalidated by mutating the state.
    pub(super) fn parents(&self, id: NodeId, node: &dyn Node) -> HashSet<NodeId> {
        let note_parents = node
            .unauthenticated_note_commitments()
            .filter_map(|note| self.output_notes.get(&note));

        let account_parents = node
            .account_updates()
            .filter_map(|(account, from, to)| {
                self.accounts.get(&account).map(|account| account.parents(from, to))
            })
            .flatten();

        account_parents
            .chain(note_parents)
            .copied()
            // Its possible for a node to have internal state connecting to itself. For example,
            // a proposed batch has not erased the internally produced and consumed notes.
            .filter(|parent| parent != &id)
            .collect()
    }

    /// The [`NodeId`]s which depend directly on the given node.
    ///
    /// Note that the result is invalidated by mutating the state.
    pub(super) fn children(&self, id: NodeId, node: &dyn Node) -> HashSet<NodeId> {
        let note_children = node
            .output_note_commitments()
            .filter_map(|note| self.unauthenticated_notes.get(&note));

        let account_children = node
            .account_updates()
            .filter_map(|(account, from, to)| {
                self.accounts.get(&account).map(|account| account.children(from, to))
            })
            .flatten();

        account_children
            .chain(note_children)
            .copied()
            // Its possible for a node to have internal state connecting to itself. For example,
            // a proposed batch has not erased the internally produced and consumed notes.
            .filter(|child| child != &id)
            .collect()
    }

    pub(super) fn inject_telemetry(&self, span: &tracing::Span) {
        use miden_node_utils::tracing::OpenTelemetrySpanExt;

        span.set_attribute("mempool.accounts", self.accounts.len());
        span.set_attribute("mempool.nullifiers", self.nullifiers.len());
        span.set_attribute("mempool.output_notes", self.output_notes.len());
    }
}

/// The commitment updates made to a account.
#[derive(Clone, Debug, PartialEq, Default)]
struct AccountUpdates {
    from: HashMap<Word, NodeId>,
    to: HashMap<Word, NodeId>,
    /// This holds updates from nodes where the initial commitment is the same as the final
    /// commitment aka no actual change was made to the account.
    ///
    /// This sounds counter-intuitive, but is caused by so-called pass-through transactions which
    /// use an account at some state `A` but only consume and emit notes without changing the
    /// account state itself.
    ///
    /// These still need to be tracked as part of account updates since they require that an
    /// account is in the given state. Since we want these node's to be processed before the
    /// account state is changed, this implies that they must be considered children of the
    /// non-pass-through node that created the state. Similarly, they must be considered
    /// parents of any non-pass-through node that changes to another state as otherwise this
    /// node might be processed before the pass-through nodes are.
    ///
    /// Pass-through nodes with the same state are considered siblings of each as they don't
    /// actually depend on each other, and may be processed in any order.
    ///
    /// Note also that its possible for any node's updates to an account to solely consist of
    /// pass-through transactions and therefore in turn is a pass-through node from the perspective
    /// of that account.
    pass_through: HashMap<Word, HashSet<NodeId>>,
}

impl AccountUpdates {
    fn latest_commitment(&self) -> Word {
        // The latest commitment will be whichever commitment isn't consumed aka a `to` which has
        // no `from`. This breaks if this isn't in a valid state.
        self.to
            .keys()
            .find(|commitment| !self.from.contains_key(commitment))
            .or(self.pass_through.keys().next())
            .copied()
            .unwrap_or_default()
    }

    fn is_empty(&self) -> bool {
        self.from.is_empty() && self.to.is_empty() && self.pass_through.is_empty()
    }

    fn remove(&mut self, id: NodeId, from: Word, to: Word) {
        if from == to {
            let entry = self.pass_through.entry(from).or_default();
            assert!(
                entry.remove(&id),
                "Account pass through commitment removal of {from} for {id:?} does not exist"
            );
            if entry.is_empty() {
                self.pass_through.remove(&from);
            }
        } else {
            let from_removed = self
                .from
                .remove(&from)
                .expect("should only be removing account updates from nodes that are present");
            let to_removed = self
                .to
                .remove(&to)
                .expect("should only be removing account updates from nodes that are present");
            assert_eq!(
                from_removed, to_removed,
                "Account updates should be removed as a pair with the same node ID"
            );
            assert_eq!(from_removed, id, "Account update removal should match the input node ID",);
        }
    }

    fn insert(&mut self, id: NodeId, from: Word, to: Word) {
        if from == to {
            assert!(
                self.pass_through.entry(from).or_default().insert(id),
                "Account already contained the pass through commitment {from} for node {id:?}"
            );
        } else {
            assert!(
                self.from.insert(from, id).is_none(),
                "Account already contained the commitment {from} when inserting {id:?}"
            );
            assert!(
                self.to.insert(to, id).is_none(),
                "Account already contained the commitment {to} when inserting {id:?}"
            );
        }
    }

    /// Returns the node IDs that updated this account's commitment to the given value.
    ///
    /// Note that this might be multiple IDs due to pass through transactions. When the input
    /// is itself a pass through transaction (`from == to`), then its sibling pass through
    /// transactions are not considered parents as they are siblings.
    ///
    /// In other words, this returns the IDs of `node` where `node.to == from`. This infers the
    /// parent-child relationship where `parent.to == child.from`.
    fn parents(&self, from: Word, to: Word) -> impl Iterator<Item = &NodeId> {
        let direct_parent = self.to.get(&from).into_iter();

        // If the node query isn't for a pass-through node, then it must also consider pass-through
        // nodes at its `from` commitment as parents.
        //
        // This means the query node depends on the pass-through nodes since these must be processed
        // before the account commitment may change.
        let pass_through_parents = (from != to)
            .then(|| self.pass_through.get(&from).map(HashSet::iter))
            .flatten()
            .unwrap_or_default();

        direct_parent.chain(pass_through_parents)
    }

    /// Returns the node ID that consumed the given commitment.
    ///
    /// Note that this might be multiple IDs due to pass through transactions. When the input
    /// is itself a pass through transaction (`from == to`), then its sibling pass through
    /// transactions are not considered children as they are siblings.
    ///
    /// In other words, this returns the ID of `node` where `node.from == to`. This infers the
    /// parent-child relationship where `parent.to == child.from`.
    fn children(&self, from: Word, to: Word) -> impl Iterator<Item = &NodeId> {
        let direct_child = self.from.get(&to).into_iter();

        // If the node query isn't for a pass-through node, then it must also consider pass-through
        // nodes at its `to` commitment as children.
        //
        // This means the pass-through nodes depend on the query node since it changes the account
        // commitment to the state required by the pass-through nodes.
        let pass_through_children = (from != to)
            .then(|| self.pass_through.get(&to).map(HashSet::iter))
            .flatten()
            .unwrap_or_default();

        direct_child.chain(pass_through_children)
    }
}
