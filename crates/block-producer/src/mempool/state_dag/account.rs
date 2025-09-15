use std::collections::VecDeque;

use miden_objects::Word;

use super::{Committed, GraphResult, NodeId};

/// Holds the state commitment transitions for a single account.
///
/// The latest committed commitment is also retained.
#[derive(Clone)]
pub(crate) struct AccountState {
    /// The latest committed account commitment.
    pub(crate) committed: Committed<Word>,

    /// Uncommitted account transitions in chronological order.
    ///
    /// Represented as a chain of `(node ID, from -> to)` commitments, with the first entry's
    /// `from` matching `self.committed`, and each subsequent entry having
    /// `transitions[n].to == transitions[n+1].from`.
    pub(crate) transitions: VecDeque<(NodeId, Word, Word)>,
}

impl AccountState {
    pub(crate) fn with_store_state(committed: Word) -> Self {
        Self {
            committed: Committed::Pruned(committed),
            transitions: VecDeque::default(),
        }
    }

    /// The `NodeId` of the `Node` which consumed the given commitment.
    ///
    /// aka where `from == commitment`.
    ///
    /// Note that unlike most other methods this does not check the commitment actually exists. In
    /// other words, `None` could also indicate that the given commitment isn't present at all.
    pub(crate) fn child(&self, commitment: Word) -> Option<NodeId> {
        self.transitions
            .iter()
            .find_map(|(id, from, _to)| (from == &commitment).then_some(*id))
    }

    /// The `NodeId` of the `Node` which created the given commitment.
    ///
    /// aka where `to == commitment`.
    ///
    /// Note that unlike most other methods this does not check the commitment actually exists. In
    /// other words, `None` could also indicate that the given commitment isn't present at all.
    pub(crate) fn parent(&self, commitment: Word) -> Option<NodeId> {
        self.transitions
            .iter()
            .find_map(|(id, _from, to)| (to == &commitment).then_some(*id))
    }

    /// Appends the account transition to the state.
    ///
    /// # Errors
    ///
    /// Returns an error if the latest account commitment does not match `from`.
    pub(crate) fn append(&mut self, id: NodeId, from: Word, to: Word) -> GraphResult<()> {
        let current = self
            .transitions
            .back()
            .map(|(_id, _from, to)| to)
            .unwrap_or(self.committed.inner());

        if current != &from {
            todo!("return error");
        }

        self.transitions.push_back((id, from, to));

        Ok(())
    }

    /// Reverts the given account transition from the account state.
    ///
    /// Expects that reversions are applied in reverse chronological order.
    ///
    /// # Errors
    ///
    /// Returns an error if this is not the latest account transition.
    pub(crate) fn revert(&mut self, id: NodeId, from: Word, to: Word) -> GraphResult<()> {
        if self.transitions.back().is_none_or(|back| back != &(id, from, to)) {
            todo!("return error");
        }

        self.transitions.pop_back();

        Ok(())
    }

    /// Commits the given account transition.
    ///
    /// The node will no longer be considered for `Self::parent` and `Self::child` methods, and can
    /// now be pruned.
    ///
    /// # Errors
    ///
    /// Errors if the oldest account transition does not match the one provided.
    pub(crate) fn commit(&mut self, id: NodeId, from: Word, to: Word) -> GraphResult<()> {
        if self.transitions.front().is_none_or(|first| first != &(id, from, to)) {
            todo!("return error");
        }

        let front = self.transitions.pop_front().unwrap();
        self.committed = Committed::Recent(front.2);

        Ok(())
    }

    /// Marks the latest commited commitment as pruned _iff_ it matches the given commitment.
    pub(crate) fn prune(&mut self, commitment: Word) {
        if self.committed.inner() == &commitment {
            self.committed = Committed::Pruned(commitment)
        }
    }

    /// Returns `true` if this [`AccountState`] has no inflight account transitions, and the
    /// committed state has been pruned.
    pub(crate) fn is_unused(&self) -> bool {
        self.committed.is_pruned() && self.transitions.is_empty()
    }
}
