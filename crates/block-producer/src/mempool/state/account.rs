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

#[derive(Clone, Debug, PartialEq)]
pub(super) struct AccountDelta {
    pub(super) id: NodeId,
    pub(super) from: Word,
    pub(super) to: Word,
}

impl AccountDelta {
    fn is_pass_through(&self) -> bool {
        self.from == self.to
    }
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

    fn contains_pass_through(&self, target: &NodeId) -> bool {
        self.pass_through.contains(target)
    }
}

impl AccountStates {
    pub(super) fn new(delta: AccountDelta) -> Self {
        assert_ne!(delta.id, Update::INVALID);

        let mut output = Self {
            updates: [Update::new(delta.from, Update::INVALID)].into(),
        };

        if delta.is_pass_through() {
            // SAFETY: We just created element [0].
            output.updates[0].pass_through.insert(delta.id);
        } else {
            output.updates.push_back(Update::new(delta.to, delta.id));
        }

        output
    }

    /// Appends the account update as the latest account commitment _IFF_ `from` matches the current
    /// account commitment.
    ///
    /// # Errors
    ///
    /// Returns the current account commitment if the above precondition does not hold.
    pub(super) fn append(&mut self, delta: AccountDelta) -> Result<(), Word> {
        // SAFETY: We are guaranteed to always have at least one element.
        let latest = self.updates.back().unwrap().commitment;
        if latest != delta.from {
            return Err(latest);
        }

        if delta.is_pass_through() {
            // SAFETY: We are guaranteed to always have at least one element.
            self.updates.back_mut().unwrap().pass_through.insert(delta.id);
        } else {
            self.updates.push_back(Update::new(delta.to, delta.id));
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
    pub(super) fn unfold(&mut self, target: NodeId, unfolded: Vec<AccountDelta>) {
        let mut unfolded = unfolded.into_iter();
        let first = unfolded.next().expect("cannot unfold an empty list of nodes");
        let mut unfolded_account = Self::new(first);
        for delta in unfolded {
            assert_ne!(delta.id, target);
            unfolded_account.append(delta).expect("unfold nodes must form a valid sequence");
        }

        let unfolded_updates = unfolded_account.updates.make_contiguous();

        let (front, rest) = unfolded_updates.split_first_mut().unwrap();
        let istart = self
            .updates
            .iter()
            .position(|update| update.commitment == front.commitment)
            .expect("unfold first commitment must exist");
        assert!(self.updates[istart].pass_through.is_disjoint(&front.pass_through));

        match rest.last_mut() {
            None => {
                // Unfolded consists of only [front] and therefore only contains pass through nodes.
                // This in turn means target must be a pass through node itself at istart.
                assert!(self.updates[istart].pass_through.remove(&target));
            },
            Some(back) => {
                // Unfolded consists of a transition of [front, .., back] and therefore target must
                // be the cause of front.commitment -> back.commitment at updates[istart + 1].
                let target_update = &mut self.updates[istart + 1];

                assert_eq!(target_update.commitment, back.commitment);
                assert_eq!(target_update.by, target);
                assert!(target_update.pass_through.is_disjoint(&back.pass_through));

                back.pass_through.extend(&target_update.pass_through);

                let mut suffix = self.updates.split_off(istart + 1);
                suffix.pop_front();

                self.updates.extend(rest.as_ref().iter().cloned());
                self.updates.extend(suffix);
            },
        }

        // This always has to happen.
        for x in &front.pass_through {
            self.updates[istart].pass_through.insert(*x);
        }
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
    pub(super) fn fold(&mut self, targets: Vec<AccountDelta>, folded: NodeId) {
        // Check that target sequence order is valid by reconstructing it.
        let mut targets = targets.into_iter();
        let first = targets.next().expect("cannot fold an empty list of nodes");
        let mut check = Self::new(first);
        for delta in targets {
            assert_ne!(delta.id, folded);
            check.append(delta).expect("fold targets must form a valid sequence");
        }

        // Verify the input updates against the accoaunt state.
        let check: &[Update] = check.updates.make_contiguous();

        // First element may contain pass-through nodes where we must ensure they exist in self.
        //
        // SAFETY: first update is guaranteed by Self since it cannot be empty.
        let (front, rest) = check.split_first().unwrap();
        let istart = self
            .updates
            .iter()
            .position(|update| update.commitment == front.commitment)
            .expect("fold target node must exist");
        assert!(self.updates[istart].pass_through.is_superset(&front.pass_through));

        if let Some((back, rest)) = rest.split_last() {
            // middle elements must match self exactly since we can't leave any node behind.
            let expected = self.updates.iter().skip(istart + 1).take(rest.len());
            itertools::assert_equal(expected, rest.iter());

            // Last element is allowed to miss some of the pass through nodes.
            let expected = &self.updates[istart + rest.len() + 1];
            assert_eq!(expected.by, back.by);
            assert_eq!(expected.commitment, back.commitment);
            assert!(expected.pass_through.is_superset(&back.pass_through));
        }

        // Actually perform the folding.
        for target in &front.pass_through {
            self.updates[istart].pass_through.remove(target);
        }

        let Some((back, rest)) = rest.split_last() else {
            // If there were only the pass through nodes then `folded` should replace them and we
            // are done.
            self.updates[istart].pass_through.insert(folded);
            return;
        };

        // Remove all of the rest updates entirely.
        if !rest.is_empty() {
            self.updates.drain(istart + 1..istart + rest.len() + 1);
        }

        // Update the "back" element which should now be next to "front".
        self.updates[istart + 1].by = folded;
        for target in &back.pass_through {
            self.updates[istart + 1].pass_through.remove(target);
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::Strategy;
    use proptest_derive::Arbitrary;
    use rand::Rng;

    use super::*;

    #[derive(Arbitrary, Debug, Clone)]
    struct ArbitraryWord {
        #[proptest(strategy = "word_strategy()")]
        inner: Word,
    }

    fn word_strategy() -> impl Strategy<Value = Word> {
        use miden_protocol::Felt;

        // Since we are using this to represent commitments, and ZERO has a special meaning there,
        // lets avoid generating (0,0,0,0).
        ((1u32..), (0u32..), (0u32..), (0u32..)).prop_map(|(a, b, c, d)| {
            let felts = [Felt::from(a), Felt::from(b), Felt::from(c), Felt::from(d)];
            Word::from(felts)
        })
    }

    #[derive(Debug, Clone)]
    struct UpdateSequence {
        updates: Vec<AccountDelta>,
    }

    impl UpdateSequence {
        fn strategy(updates: usize, passthrough_chance: f64) -> impl Strategy<Value = Self> {
            use proptest::prelude::*;

            let offset = (1u32..10_000).prop_map(|offset| BlockNumber::from(offset));
            let init = ArbitraryWord::arbitrary();

            let updates = proptest::collection::vec(ArbitraryWord::arbitrary(), updates);

            (offset, init, updates).prop_perturb(move |(offset, init, deltas), mut rng| {
                let mut updates = Vec::with_capacity(deltas.len());
                let mut next_from = init.inner;
                let mut next_id = offset;

                for commitment in deltas {
                    let from = next_from;
                    let id = NodeId::Block(next_id);

                    let to = if rng.random_bool(passthrough_chance) {
                        next_from
                    } else {
                        commitment.inner
                    };

                    next_from = to;
                    next_id = next_id.child();

                    updates.push(AccountDelta { id, from, to });
                }

                UpdateSequence { updates }
            })
        }
    }

    proptest::proptest! {
        /// Folds and unfolds a range within an arbitrary, but valid sequence of updates.
        ///
        /// We ensure the following properties hold:
        ///
        /// - Folding [x] into y is equivalent inserting y and not [x].
        /// - Folding [x] into y and then unfolding y into [x] is a noop.
        #[test]
        fn refolding_is_a_noop(
            (sequence, fold) in (1..100usize)
                .prop_flat_map(|n| UpdateSequence::strategy(n, 0.33))
                .prop_perturb(|sequence, mut rng| {
                    let begin = rng.random_range(0..sequence.updates.len());
                    let end = rng.random_range(begin + 1..sequence.updates.len() + 1);
                    (sequence, begin..end)
                }))
        {
            let (init, tail) = sequence.updates.split_first().unwrap();
            let mut uut = AccountStates::new(init.clone());
            for delta in tail {
                uut.append(delta.clone()).expect("appending to uut");
            }
            let original = uut.clone();

            let fold_targets = sequence.updates[fold.clone()].to_vec();
            let fold_into = AccountDelta {
                id: NodeId::Block(u32::MAX.into()),
                from: fold_targets.first().unwrap().from,
                to: fold_targets.last().unwrap().to,
            };

            let (prelude, _) = sequence.updates.split_at(fold.start);
            let (_, epilogue) = sequence.updates.split_at(fold.end);
            let mut reference = prelude.to_vec();
            reference.push(fold_into.clone());
            reference.extend(epilogue.into_iter().cloned());

            let (init, tail) = reference.split_first().unwrap();
            let mut reference = AccountStates::new(init.clone());
            for delta in tail {
                reference.append(delta.clone()).expect("appending to reference");
            }

            uut.fold(fold_targets.clone(), fold_into.id);
            proptest::prop_assert_eq!(&uut, &reference);

            // uut.unfold(fold_into.id, fold_targets);
            // proptest::prop_assert_eq!(uut, original);
        }
    }
}
