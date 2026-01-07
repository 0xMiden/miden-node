#![allow(dead_code, reason = "unused as the main refactor is wip")]

use std::collections::{HashSet, VecDeque};

use itertools::Itertools;
use miden_protocol::Word;
use miden_protocol::block::BlockNumber;

use crate::mempool::NodeId;

/// The commitment updates made to an account.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct AccountStates {
    /// A sequential series of updates to this account.
    ///
    /// It is guaranteed to have at least one entry. This is enforced by only allowing a non-empty
    /// set to be created, and removal of an update returns `None` if the account is empty.
    ///
    /// The first (`updates[0]`) update's `by` field is always invalid, set to [`Update::INVALID`]
    /// and is ignored internally. See the [`evict`](Self::evict) and [`prune`](Self::prune)
    /// methods.
    ///
    /// An alternate representation would be splitting `updates[0]` out into its own field without
    /// `by`, but in practice this caused much more code complexity in the rest of the
    /// implementation.
    updates: VecDeque<Update>,
}

/// Describes a state transition of an account.
#[derive(Clone, Debug, PartialEq)]
struct Update {
    /// The account state commitment.
    commitment: Word,
    /// The node which caused the transition to `commitment` state.
    by: NodeId,
    /// Nodes which have both initial and final state `commitment`. These are known as pass through
    /// nodes.
    ///
    /// These are modelled as being "siblings" without any dependency between each other. This is
    /// correct from the perspective of the account, but note that these can have dependencies
    /// caused by notes (this is handled externally).
    pass_through: HashSet<NodeId>,
}

impl Update {
    /// Used to indicate that an update's `by` field should be ignored.
    ///
    /// Set to the genesis block number as this ID will never be used in practice since the genesis
    /// block is never inflight.
    const INVALID: NodeId = NodeId::Block(BlockNumber::GENESIS);

    /// Creates a new update with no pass through nodes.
    fn new(commitment: Word, by: NodeId) -> Self {
        Self {
            commitment,
            by,
            pass_through: HashSet::default(),
        }
    }
}

impl AccountStates {
    pub(super) fn new(id: NodeId, from: Word, to: Word) -> Self {
        let mut output = Self {
            updates: [Update::new(from, Update::INVALID)].into(),
        };

        if from == to {
            // SAFETY: We just created element [0].
            output.updates[0].pass_through.insert(id);
        } else {
            output.updates.push_back(Update::new(to, id));
        }

        output
    }

    /// Appends the account update as the latest account commitment _IFF_ `from` matches the current
    /// account commitment.
    ///
    /// # Errors
    ///
    /// Returns the current account commitment if the above precondition does not hold.
    pub(super) fn append(&mut self, id: NodeId, from: Word, to: Word) -> Result<(), Word> {
        // SAFETY: We are guaranteed to always have at least one element.
        let latest = self.updates.back().unwrap().commitment;
        if latest != from {
            return Err(latest);
        }

        if from == to {
            // SAFETY: We are guaranteed to always have at least one element.
            self.updates.back_mut().unwrap().pass_through.insert(id);
        } else {
            self.updates.push_back(Update::new(to, id));
        }

        Ok(())
    }

    /// Evicts the youngest node from the account state. Note that this title may be shared if the
    /// youngest commitment has multiple pass-through nodes.
    ///
    /// Returns `None` if the account is now empty, otherwise returns the modified account state.
    ///
    /// # Panics
    ///
    /// Panics if the node has any descendents i.e. if its not the youngest update.
    #[must_use = "account may or may not be empty"]
    pub(super) fn evict(mut self, id: NodeId) -> Option<Self> {
        // SAFETY: we are guaranteed to always have at least one element.
        let last = self.updates.back_mut().unwrap();

        if last.pass_through.is_empty() {
            assert!(last.by == id, "failed to evict {id:?} from account");
            // This will never pop the [0] element as [0].by is always set to an unmatchable ID.
            self.updates.pop_back();
        } else {
            assert!(last.pass_through.remove(&id), "failed to evict {id:?} from account");
        }

        self.discard_if_empty()
    }

    /// Prunes the oldest node from the account state. Note that this title may be shared if the
    /// oldest commitment has multiple pass-through nodes.
    ///
    /// Returns `None` if the account is now empty, otherwise returns the modified account state.
    ///
    /// # Panics
    ///
    /// Panics if the node has any ancestors in the account i.e. if its not the oldest update.
    #[must_use = "account may or may not be empty"]
    pub(super) fn prune(mut self, id: NodeId) -> Option<Self> {
        // SAFETY: we are guaranteed to always have at least one element.
        let front = self.updates.front_mut().unwrap();

        if front.pass_through.is_empty() {
            // SAFETY: Since the first element is empty, the second element must also exist.
            assert!(self.updates[1].by == id, "failed to prune {id:?} from account");
            self.updates.pop_front();
            self.updates.front_mut().unwrap().by = Update::INVALID;
        } else {
            assert!(front.pass_through.remove(&id), "failed to prune {id:?} from account");
        }

        self.discard_if_empty()
    }

    /// Returns `None` if the account has no inflight updates remaining, otherwise returns itself.
    ///
    /// This helper method enforces type safety by forcing the caller to reevaluate the account
    /// after an update has been removed.
    fn discard_if_empty(self) -> Option<Self> {
        // The state is empty if the [0] has no pass through nodes _and_ there is no next update.
        (self.updates.len() > 1 || !self.updates[0].pass_through.is_empty()).then_some(self)
    }

    /// The nodes IDs that the given node depends on.
    ///
    /// These are essentially the nodes that must be committed _before_ the target node is with
    /// respect to this account state. In practice this will usually be one or zero nodes, but it
    /// may be multiple due to pass through transactions.
    ///
    /// For a target node with account update `b -> c`:
    /// - if `b == c` (target is pass through node) it returns the node which caused `a -> b`
    /// - otherwise it returns node `a -> b` and all pass through nodes `b -> b`
    ///
    /// # Panics
    ///
    /// Panics if the target node does not exist.
    pub(super) fn depends_on(&self, target: NodeId) -> HashSet<NodeId> {
        for (from, to) in self.updates.iter().tuple_windows() {
            if to.pass_through.contains(&target) {
                // A pass through node only depends on the node that created its state.
                return [to.by].into();
            } else if to.by == target {
                // Non-pass through nodes depend on the node that caused the previous transition,
                // and also any pass through nodes from the target's origin state.
                //
                // This is because we want these pass through nodes to be processed _before_ the
                // target's update is applied.
                let mut output = from.pass_through.clone();
                // Don't insert [0].by
                if from.by != Update::INVALID {
                    output.insert(from.by);
                }

                return output;
            }
        }

        // Check the first element which the above loop skips. The first element can only contain
        // pass through nodes, and by definition these can't depend on anything if they do exist.
        //
        // SAFETY: We are guaranteed at least one element.
        if self.updates.front().unwrap().pass_through.contains(&target) {
            return HashSet::default();
        }

        panic!("Target node {target:?} not found");
    }

    /// Nodes for which the given node is a pre-requisite.
    ///
    /// Essentially nodes which can only be considered once the _target_ node has been processed. In
    /// practice this will usually be one or zero nodes, but it may be multiple due to pass
    /// through transactions.
    ///
    /// For a target node with account update `a -> b`:
    /// - if `a == b` (target is pass through node) it returns the node `b -> c`
    /// - otherwise it returns node `b -> c` and all pass through nodes `b -> b`
    ///
    /// # Panics
    ///
    /// Panics if the target node does not exist.
    pub(super) fn required_by(&self, target: NodeId) -> HashSet<NodeId> {
        for (from, to) in self.updates.iter().tuple_windows() {
            if from.by == target {
                let mut output = from.pass_through.clone();
                output.insert(to.by);
                return output;
            } else if from.pass_through.contains(&target) {
                return [to.by].into();
            }
        }

        // Check the last element which the above loop skips.
        //
        // SAFETY: We are guaranteed at least one element.
        let last = self.updates.back().unwrap();
        if last.by == target {
            return last.pass_through.clone();
        }
        if last.pass_through.contains(&target) {
            return HashSet::default();
        }

        panic!("Target node {target:?} not found");
    }

    /// Replaces the existing target node with a series of equivalent updates.
    ///
    /// This is intended to allow "unfolding" a batch or block's update into its constituent
    /// transactions.
    ///
    /// # Panics
    ///
    /// Panics if
    ///
    /// - the target node cannot be found
    /// - the unfolded nodes aren't sequential
    /// - unfolded nodes beginning and end don't match the target
    pub(super) fn unfold(&mut self, _target: NodeId, _unfolded: Vec<(NodeId, Word, Word)>) {
        todo!();
    }

    /// Replaces existing sequential target nodes with a single node.
    ///
    /// This is intended to allow "folding" transactions into batch or batches into a  block.
    ///
    /// # Panics
    ///
    /// Panics if
    ///
    /// - the target nodes cannot be found
    /// - the target nodes aren't sequential
    /// - some nodes are left behind (e.g. pass through nodes are skipped)
    pub(super) fn fold(&mut self, _targets: Vec<(NodeId, Word, Word)>, _folded: NodeId) {
        todo!();
    }
}
