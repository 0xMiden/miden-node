#![allow(dead_code, reason = "unused as the main refactor is wip")]

use std::collections::{HashSet, VecDeque};

use miden_protocol::Word;

use crate::mempool::NodeId;

/// The commitment updates made to an account.
///
/// Tracks a chain of state transitions starting from an anchor (committed) state.
/// The structure guarantees that either the anchor has pass-through nodes, or there
/// is at least one transition - this ensures the account has meaningful in-flight state.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct AccountStates {
    /// The anchor state - the initial committed commitment before any in-flight updates.
    anchor: Anchor,
    /// Sequential transitions from the anchor state. May be empty if only pass-through
    /// nodes exist at the anchor.
    transitions: VecDeque<Transition>,
}

/// The anchor (starting) state for an account's in-flight updates.
#[derive(Clone, Debug, PartialEq)]
struct Anchor {
    /// The committed account state commitment.
    commitment: Word,
    /// Pass-through nodes at the anchor state (nodes that use but don't change the state).
    pass_through: HashSet<NodeId>,
}

/// A state transition caused by a node.
#[derive(Clone, Debug, PartialEq)]
struct Transition {
    /// The account state commitment after this transition.
    to: Word,
    /// The node which caused the transition to this state.
    by: NodeId,
    /// Pass-through nodes at this state.
    pass_through: HashSet<NodeId>,
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

impl AccountStates {
    pub(super) fn new(delta: &AccountDelta) -> Self {
        let mut anchor = Anchor {
            commitment: delta.from,
            pass_through: HashSet::default(),
        };

        let transitions = if delta.is_pass_through() {
            anchor.pass_through.insert(delta.id);
            VecDeque::new()
        } else {
            [Transition {
                to: delta.to,
                by: delta.id,
                pass_through: HashSet::default(),
            }]
            .into()
        };

        Self { anchor, transitions }
    }

    /// Appends the account update as the latest account commitment _IFF_ `from` matches the current
    /// account commitment.
    ///
    /// # Errors
    ///
    /// Returns the current account commitment if the above precondition does not hold.
    pub(super) fn append(&mut self, delta: &AccountDelta) -> Result<(), Word> {
        let latest = self.latest_commitment();
        if latest != delta.from {
            return Err(latest);
        }

        if delta.is_pass_through() {
            self.latest_pass_through_mut().insert(delta.id);
        } else {
            self.transitions.push_back(Transition {
                to: delta.to,
                by: delta.id,
                pass_through: HashSet::default(),
            });
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
        match self.transitions.back_mut() {
            Some(last) if last.pass_through.contains(&id) => {
                last.pass_through.remove(&id);
            },
            Some(last) if last.by == id => {
                self.transitions.pop_back();
            },
            Some(_) => panic!("failed to evict {id:?} from account: not the youngest"),
            None => {
                // No transitions, must be in anchor pass-through
                assert!(
                    self.anchor.pass_through.remove(&id),
                    "failed to evict {id:?} from account"
                );
            },
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
        if self.anchor.pass_through.contains(&id) {
            self.anchor.pass_through.remove(&id);
        } else if let Some(first) = self.transitions.front() {
            assert_eq!(first.by, id, "failed to prune {id:?} from account: not the oldest");
            let removed = self.transitions.pop_front().unwrap();
            self.anchor = Anchor {
                commitment: removed.to,
                pass_through: removed.pass_through,
            };
        } else {
            panic!("failed to prune {id:?} from account: node not found");
        }

        self.discard_if_empty()
    }

    /// Returns `None` if the account has no inflight updates remaining, otherwise returns itself.
    fn discard_if_empty(self) -> Option<Self> {
        let is_empty = self.transitions.is_empty() && self.anchor.pass_through.is_empty();
        (!is_empty).then_some(self)
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
        // Check anchor pass-through first
        if self.anchor.pass_through.contains(&target) {
            return HashSet::default();
        }

        let mut prev_pass_through = &self.anchor.pass_through;
        let mut prev_by: Option<NodeId> = None;

        for transition in &self.transitions {
            if transition.pass_through.contains(&target) {
                // Pass-through node only depends on the node that created this state
                return HashSet::from_iter(prev_by);
            }

            if transition.by == target {
                // Non-pass-through depends on previous transition's creator and all
                // pass-through nodes at the previous state
                let mut deps = prev_pass_through.clone();
                if let Some(by) = prev_by {
                    deps.insert(by);
                }
                return deps;
            }

            prev_pass_through = &transition.pass_through;
            prev_by = Some(transition.by);
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
        // Check anchor pass-through
        if self.anchor.pass_through.contains(&target) {
            // Anchor pass-through is required by the first transition (if any)
            return HashSet::from_iter(self.transitions.front().map(|t| t.by));
        }

        for (i, transition) in self.transitions.iter().enumerate() {
            if transition.by == target {
                // This transition's creator is required by pass-throughs at this state
                // and the next transition (if any)
                let mut required = transition.pass_through.clone();
                if let Some(next) = self.transitions.get(i + 1) {
                    required.insert(next.by);
                }
                return required;
            }

            if transition.pass_through.contains(&target) {
                // A pass-through node at this state is only required by the node that causes
                // the next state transition. If there is no next transition, the pass-through
                // has no dependents from this account's perspective.
                return HashSet::from_iter(self.transitions.get(i + 1).map(|t| t.by));
            }
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
    pub(super) fn unfold(&mut self, target: NodeId, unfolded: &[AccountDelta]) {
        let replacement = Self::from_deltas(unfolded);

        // Find where target is located - could be in anchor pass-through, transition.by,
        // or transition.pass_through

        // Case 1: Target is anchor pass-through
        if self.anchor.pass_through.contains(&target) {
            // Replacement must also be all pass-through at anchor commitment
            assert!(
                replacement.transitions.is_empty(),
                "cannot unfold anchor pass-through into transitions"
            );
            assert_eq!(
                replacement.anchor.commitment, self.anchor.commitment,
                "unfold commitment mismatch"
            );

            self.anchor.pass_through.remove(&target);
            self.anchor.pass_through.extend(&replacement.anchor.pass_through);
            return;
        }

        // Case 2: Target is a transition pass-through
        if let Some(idx) = self.transitions.iter().position(|t| t.pass_through.contains(&target)) {
            // Target is pass-through at transitions[idx].to commitment
            let commitment_at = self.transitions[idx].to;

            // Replacement must also be all pass-through at the same commitment
            assert!(
                replacement.transitions.is_empty(),
                "cannot unfold pass-through into transitions"
            );
            assert_eq!(
                replacement.anchor.commitment, commitment_at,
                "unfold commitment mismatch"
            );

            self.transitions[idx].pass_through.remove(&target);
            self.transitions[idx]
                .pass_through
                .extend(&replacement.anchor.pass_through);
            return;
        }

        // Case 3: Target is a transition.by
        let target_idx = self
            .transitions
            .iter()
            .position(|t| t.by == target)
            .expect("unfold target not found");

        // Capture target's data before modifying
        let target_to = self.transitions[target_idx].to;
        let target_pass_through = self.transitions[target_idx].pass_through.clone();

        // Validate replacement matches target's transition
        let prev_commitment = if target_idx == 0 {
            self.anchor.commitment
        } else {
            self.transitions[target_idx - 1].to
        };
        assert_eq!(replacement.anchor.commitment, prev_commitment, "unfold from mismatch");

        if replacement.transitions.is_empty() {
            // Replacement is all pass-through at prev_commitment
            // This means target was effectively a pass-through (from == to)
            assert_eq!(target_to, prev_commitment, "cannot unfold transition into pass-throughs");

            // Add replacement pass-throughs to previous location
            if target_idx == 0 {
                self.anchor.pass_through.extend(&replacement.anchor.pass_through);
            } else {
                self.transitions[target_idx - 1]
                    .pass_through
                    .extend(&replacement.anchor.pass_through);
            }

            // Remove the target transition and redistribute its pass-throughs
            self.transitions.remove(target_idx);
            if target_idx > 0 {
                self.transitions[target_idx - 1]
                    .pass_through
                    .extend(target_pass_through);
            } else if !self.transitions.is_empty() {
                // Target was first transition, pass-throughs go to anchor
                // (but they were at target_to which equals prev_commitment = anchor.commitment)
                self.anchor.pass_through.extend(target_pass_through);
            } else {
                self.anchor.pass_through.extend(target_pass_through);
            }
        } else {
            // Replacement has transitions
            let replacement_final_to = replacement.transitions.back().unwrap().to;
            assert_eq!(replacement_final_to, target_to, "unfold to mismatch");

            // Merge anchor pass-throughs into previous location
            if target_idx == 0 {
                self.anchor.pass_through.extend(&replacement.anchor.pass_through);
            } else {
                self.transitions[target_idx - 1]
                    .pass_through
                    .extend(&replacement.anchor.pass_through);
            }

            // Remove target transition
            self.transitions.remove(target_idx);

            // Insert replacement transitions
            let num_replacements = replacement.transitions.len();
            for (i, mut t) in replacement.transitions.into_iter().enumerate() {
                // Last replacement inherits target's pass-throughs
                if i == num_replacements - 1 {
                    t.pass_through.extend(&target_pass_through);
                }
                self.transitions.insert(target_idx + i, t);
            }
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
    pub(super) fn fold(&mut self, targets: &[AccountDelta], folded: NodeId) {
        let to_fold = Self::from_deltas(targets);

        // Find where the fold starts
        let fold_start_commitment = to_fold.anchor.commitment;

        // Check if fold starts at anchor
        let starts_at_anchor = self.anchor.commitment == fold_start_commitment;
        let start_idx = if starts_at_anchor {
            0
        } else {
            self.transitions
                .iter()
                .position(|t| t.to == fold_start_commitment)
                .map(|i| i + 1)
                .expect("fold start commitment not found")
        };

        // Validate and remove anchor pass-throughs being folded
        if starts_at_anchor {
            assert!(
                self.anchor.pass_through.is_superset(&to_fold.anchor.pass_through),
                "fold targets not found in anchor pass-through"
            );
            for id in &to_fold.anchor.pass_through {
                self.anchor.pass_through.remove(id);
            }
        } else {
            let prev = &mut self.transitions[start_idx - 1];
            assert!(
                prev.pass_through.is_superset(&to_fold.anchor.pass_through),
                "fold targets not found in pass-through"
            );
            for id in &to_fold.anchor.pass_through {
                prev.pass_through.remove(id);
            }
        }

        if to_fold.transitions.is_empty() {
            // All targets are pass-through, folded becomes pass-through
            if starts_at_anchor {
                self.anchor.pass_through.insert(folded);
            } else {
                self.transitions[start_idx - 1].pass_through.insert(folded);
            }
            return;
        }

        // Validate and remove transitions being folded
        let num_transitions = to_fold.transitions.len();
        for (i, expected) in to_fold.transitions.iter().enumerate() {
            let actual = &self.transitions[start_idx + i];
            assert_eq!(actual.by, expected.by, "fold transition mismatch at index {i}");
            assert_eq!(actual.to, expected.to, "fold commitment mismatch at index {i}");
            if i < num_transitions - 1 {
                // Middle transitions must match exactly
                assert_eq!(
                    actual.pass_through, expected.pass_through,
                    "fold pass-through mismatch at index {i}"
                );
            } else {
                // Last transition may have extra pass-throughs not being folded
                assert!(
                    actual.pass_through.is_superset(&expected.pass_through),
                    "fold targets not found in final pass-through"
                );
            }
        }

        // Get the final commitment and preserve extra pass-throughs
        let final_to = to_fold.transitions.back().unwrap().to;
        let mut preserved_pass_through = self.transitions[start_idx + num_transitions - 1]
            .pass_through
            .clone();
        for id in &to_fold.transitions.back().unwrap().pass_through {
            preserved_pass_through.remove(id);
        }

        // Remove folded transitions
        for _ in 0..num_transitions {
            self.transitions.remove(start_idx);
        }

        // Insert single folded transition
        self.transitions.insert(
            start_idx,
            Transition {
                to: final_to,
                by: folded,
                pass_through: preserved_pass_through,
            },
        );
    }

    /// Helper to get the latest commitment.
    fn latest_commitment(&self) -> Word {
        self.transitions.back().map_or(self.anchor.commitment, |t| t.to)
    }

    /// Helper to get mutable access to the latest pass-through set.
    fn latest_pass_through_mut(&mut self) -> &mut HashSet<NodeId> {
        self.transitions
            .back_mut()
            .map(|t| &mut t.pass_through)
            .unwrap_or(&mut self.anchor.pass_through)
    }

    /// Helper to construct from a sequence of deltas.
    fn from_deltas(deltas: &[AccountDelta]) -> Self {
        let mut deltas = deltas.iter();
        let first = deltas.next().expect("cannot create from empty deltas");
        let mut states = Self::new(first);
        for delta in deltas {
            states.append(delta).expect("deltas must form a valid sequence");
        }
        states
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::Strategy;
    use proptest_derive::Arbitrary;
    use miden_protocol::block::BlockNumber;
    use rand::Rng;

    use super::*;

    fn node(n: u32) -> NodeId {
        NodeId::Block(BlockNumber::from(n))
    }

    fn word(n: u32) -> Word {
        use miden_protocol::Felt;
        Word::from([Felt::from(n), Felt::from(0u32), Felt::from(0u32), Felt::from(0u32)])
    }

    /// Test basic pass-through at anchor: single pass-through node at initial state.
    #[test]
    fn passthrough_at_anchor_single() {
        let a = word(1);
        // tx1 is pass-through at anchor state A
        let delta = AccountDelta { id: node(1), from: a, to: a };
        let states = AccountStates::new(&delta);

        // Should depend on nothing (it's at the anchor)
        assert!(states.depends_on(node(1)).is_empty());
        // Should have nothing depending on it yet
        assert!(states.required_by(node(1)).is_empty());
    }

    /// Test multiple pass-through nodes at anchor.
    #[test]
    fn passthrough_at_anchor_multiple() {
        let a = word(1);
        // tx1 and tx2 are both pass-through at anchor state A
        let delta1 = AccountDelta { id: node(1), from: a, to: a };
        let delta2 = AccountDelta { id: node(2), from: a, to: a };

        let mut states = AccountStates::new(&delta1);
        states.append(&delta2).unwrap();

        // Neither depends on each other (siblings)
        assert!(states.depends_on(node(1)).is_empty());
        assert!(states.depends_on(node(2)).is_empty());
        assert!(states.required_by(node(1)).is_empty());
        assert!(states.required_by(node(2)).is_empty());
    }

    /// Test that a state-changing tx after anchor pass-throughs depends on them.
    #[test]
    fn transition_after_anchor_passthrough() {
        let a = word(1);
        let b = word(2);
        // tx1 is pass-through at A, tx2 transitions A -> B
        let delta1 = AccountDelta { id: node(1), from: a, to: a };
        let delta2 = AccountDelta { id: node(2), from: a, to: b };

        let mut states = AccountStates::new(&delta1);
        states.append(&delta2).unwrap();

        // tx1 depends on nothing
        assert!(states.depends_on(node(1)).is_empty());
        // tx2 depends on tx1 (pass-through must complete before state changes)
        assert_eq!(states.depends_on(node(2)), [node(1)].into());
        // tx1 is required by tx2
        assert_eq!(states.required_by(node(1)), [node(2)].into());
    }

    /// Test folding pass-through nodes at anchor into a single node.
    #[test]
    fn fold_anchor_passthroughs() {
        let a = word(1);
        let b = word(2);
        // tx1, tx2 pass-through at A, tx3 transitions A -> B
        let delta1 = AccountDelta { id: node(1), from: a, to: a };
        let delta2 = AccountDelta { id: node(2), from: a, to: a };
        let delta3 = AccountDelta { id: node(3), from: a, to: b };

        let mut states = AccountStates::new(&delta1);
        states.append(&delta2).unwrap();
        states.append(&delta3).unwrap();

        let original = states.clone();

        // Fold tx1 and tx2 into batch1 (pass-through)
        let batch = node(100);
        states.fold(&[delta1.clone(), delta2.clone()], batch);

        // batch should now be at anchor, tx3 depends on it
        assert!(states.depends_on(batch).is_empty());
        assert_eq!(states.depends_on(node(3)), [batch].into());

        // Unfold should restore original
        states.unfold(batch, &[delta1, delta2]);
        assert_eq!(states, original);
    }

    /// Test folding when all transactions are pass-through (no state transitions).
    #[test]
    fn fold_all_passthroughs() {
        let a = word(1);
        // tx1, tx2, tx3 are all pass-through at state A
        let delta1 = AccountDelta { id: node(1), from: a, to: a };
        let delta2 = AccountDelta { id: node(2), from: a, to: a };
        let delta3 = AccountDelta { id: node(3), from: a, to: a };

        let mut states = AccountStates::new(&delta1);
        states.append(&delta2).unwrap();
        states.append(&delta3).unwrap();

        let original = states.clone();

        // Fold all three into a single pass-through batch
        let batch = node(100);
        states.fold(&[delta1.clone(), delta2.clone(), delta3.clone()], batch);

        // batch should be alone at anchor with no dependencies
        assert!(states.depends_on(batch).is_empty());
        assert!(states.required_by(batch).is_empty());

        // Unfold should restore original
        states.unfold(batch, &[delta1.clone(), delta2.clone(), delta3.clone()]);
        assert_eq!(states, original);

        // Test unfold -> fold roundtrip starting from folded state
        let mut folded = original.clone();
        folded.fold(&[delta1.clone(), delta2.clone(), delta3.clone()], batch);
        let folded_copy = folded.clone();

        folded.unfold(batch, &[delta1.clone(), delta2.clone(), delta3.clone()]);
        assert_eq!(folded, original);

        folded.fold(&[delta1, delta2, delta3], batch);
        assert_eq!(folded, folded_copy);
    }

    /// Test unfolding a pass-through node into multiple pass-through nodes.
    #[test]
    fn unfold_all_passthroughs() {
        let a = word(1);
        // Start with a single pass-through batch at state A
        let batch = node(100);
        let batch_delta = AccountDelta { id: batch, from: a, to: a };

        let mut states = AccountStates::new(&batch_delta);
        let folded = states.clone();

        // The batch unfolds into tx1, tx2, tx3 all pass-through at A
        let delta1 = AccountDelta { id: node(1), from: a, to: a };
        let delta2 = AccountDelta { id: node(2), from: a, to: a };
        let delta3 = AccountDelta { id: node(3), from: a, to: a };

        states.unfold(batch, &[delta1.clone(), delta2.clone(), delta3.clone()]);

        // All three should be at anchor with no dependencies between them
        assert!(states.depends_on(node(1)).is_empty());
        assert!(states.depends_on(node(2)).is_empty());
        assert!(states.depends_on(node(3)).is_empty());
        assert!(states.required_by(node(1)).is_empty());
        assert!(states.required_by(node(2)).is_empty());
        assert!(states.required_by(node(3)).is_empty());

        // Fold should restore to original folded state
        states.fold(&[delta1.clone(), delta2.clone(), delta3.clone()], batch);
        assert_eq!(states, folded);

        // Test fold -> unfold roundtrip starting from unfolded state
        let mut unfolded = folded.clone();
        unfolded.unfold(batch, &[delta1.clone(), delta2.clone(), delta3.clone()]);
        let unfolded_copy = unfolded.clone();

        unfolded.fold(&[delta1.clone(), delta2.clone(), delta3.clone()], batch);
        assert_eq!(unfolded, folded);

        unfolded.unfold(batch, &[delta1, delta2, delta3]);
        assert_eq!(unfolded, unfolded_copy);
    }

    /// Test folding a mix of anchor pass-through and transition.
    #[test]
    fn fold_anchor_passthrough_and_transition() {
        let a = word(1);
        let b = word(2);
        let c = word(3);
        // tx1 pass-through at A, tx2 transitions A -> B, tx3 transitions B -> C
        let delta1 = AccountDelta { id: node(1), from: a, to: a };
        let delta2 = AccountDelta { id: node(2), from: a, to: b };
        let delta3 = AccountDelta { id: node(3), from: b, to: c };

        let mut states = AccountStates::new(&delta1);
        states.append(&delta2).unwrap();
        states.append(&delta3).unwrap();

        let original = states.clone();

        // Fold tx1 and tx2 into batch1 (A -> B, consuming the pass-through)
        let batch = node(100);
        states.fold(&[delta1.clone(), delta2.clone()], batch);

        // batch should depend on nothing (at anchor), tx3 depends on batch
        assert!(states.depends_on(batch).is_empty());
        assert_eq!(states.depends_on(node(3)), [batch].into());

        // Unfold should restore original
        states.unfold(batch, &[delta1, delta2]);
        assert_eq!(states, original);
    }

    /// Test evicting pass-through nodes at anchor.
    #[test]
    fn evict_anchor_passthrough() {
        let a = word(1);
        let delta1 = AccountDelta { id: node(1), from: a, to: a };
        let delta2 = AccountDelta { id: node(2), from: a, to: a };

        let mut states = AccountStates::new(&delta1);
        states.append(&delta2).unwrap();

        // Evict tx2, should still have tx1
        let states = states.evict(node(2)).expect("should not be empty");
        assert!(states.depends_on(node(1)).is_empty());

        // Evict tx1, should be empty
        let states = states.evict(node(1));
        assert!(states.is_none());
    }

    /// Test pruning pass-through nodes at anchor.
    #[test]
    fn prune_anchor_passthrough() {
        let a = word(1);
        let b = word(2);
        let delta1 = AccountDelta { id: node(1), from: a, to: a };
        let delta2 = AccountDelta { id: node(2), from: a, to: b };

        let mut states = AccountStates::new(&delta1);
        states.append(&delta2).unwrap();

        // Prune tx1 (anchor pass-through), tx2 should now be at anchor
        let states = states.prune(node(1)).expect("should not be empty");
        assert!(states.depends_on(node(2)).is_empty());

        // Prune tx2, should be empty
        let states = states.prune(node(2));
        assert!(states.is_none());
    }

    /// A wrapper to implement [`Arbitrary`] for [`Word`].
    ///
    /// Can be removed once upstream implements it.
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

    /// A sequence of deltas which are known to be valid.
    #[derive(Debug, Clone)]
    struct UpdateSequence {
        updates: Vec<AccountDelta>,
    }

    impl UpdateSequence {
        fn finalize(self) -> AccountStates {
            let mut updates = self.updates.into_iter();
            let first = updates.next().expect("sequence should contain at least one item");

            let mut out = AccountStates::new(&first);
            for delta in updates {
                out.append(&delta).expect("sequence must be valid");
            }
            out
        }

        /// Generates a valid sequence of `n_updates` updates, with each update having the given
        /// probability of being a pass-through node.
        fn strategy(n_updates: usize, passthrough_chance: f64) -> impl Strategy<Value = Self> {
            use proptest::prelude::*;

            // We use sequential `NodeId::Block` as an easy way to ensure that the sequence has
            // no duplicates. However, this means we might not test weird hashing or comparisons so
            // we add a random offset to the start ID to add some variation.
            let offset = (1u32..10_000).prop_map(BlockNumber::from);

            let init_commitment = ArbitraryWord::arbitrary();

            // Generate a commitment for each update. This will be the update's final commitment
            // if it isn't a pass through update.
            let updates = proptest::collection::vec(ArbitraryWord::arbitrary(), n_updates);

            // Map to a sequence of updates.
            (offset, init_commitment, updates).prop_perturb(
                move |(offset, init, deltas), mut rng| {
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
                },
            )
        }
    }

    proptest::proptest! {
        /// Folds and unfolds a range within an arbitrary, but valid sequence of updates.
        ///
        /// This also includes an arbitrary chance for each update to be a pass through update.
        ///
        /// We ensure the following properties hold:
        ///
        /// - Folding [x] into `y` is equivalent inserting `y` and skipping [x].
        /// - Folding [x] into `y` and then unfolding `y` into [x] is a noop.
        ///
        /// Along the way this of course also tests appending valid updates and pass through nodes.
        #[test]
        fn refolding_is_a_noop(
            (sequence, fold) in (1..100usize)
                .prop_flat_map(|n| UpdateSequence::strategy(n, 0.33))
                .prop_perturb(|sequence, mut rng| {
                    let begin = rng.random_range(0..sequence.updates.len());
                    let end = rng.random_range(begin + 1..sequence.updates.len() + 1);
                    (sequence, begin..end)
                })
        ) {
            let mut uut = sequence.clone().finalize();
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
            reference.extend(epilogue.iter().cloned());
            let reference = UpdateSequence { updates: reference }.finalize();

            uut.fold(&fold_targets, fold_into.id);
            proptest::prop_assert_eq!(&uut, &reference);

            uut.unfold(fold_into.id, &fold_targets);
            proptest::prop_assert_eq!(&uut, &original);

            // Also test unfold -> fold roundtrip: starting from reference (folded state),
            // unfold then fold should return to reference.
            let mut uut2 = reference.clone();
            uut2.unfold(fold_into.id, &fold_targets);
            proptest::prop_assert_eq!(&uut2, &original);

            uut2.fold(&fold_targets, fold_into.id);
            proptest::prop_assert_eq!(uut2, reference);
        }

        /// Ensures that evicting in reverse chronological order always succeeds.
        ///
        /// We also check that eviction is equivalent to never inserting the item.
        #[test]
        fn evicting_in_filo_order(
            sequence in (1..100usize)
                .prop_flat_map(|n| UpdateSequence::strategy(n, 0.66))
        ) {
            let uut = sequence.clone().finalize();
            let mut uut = Some(uut);
            let mut deltas = sequence.updates.as_slice();
            while !deltas.is_empty() {
                let reference = UpdateSequence { updates: deltas.to_vec() }.finalize();

                proptest::prop_assert_eq!(&uut, &Some(reference));

                uut = uut.expect("should still be some").evict(deltas.last().unwrap().id);
                deltas = deltas.split_last().unwrap().1;
            }

            // We should have pruned all deltas.
            proptest::prop_assert_eq!(uut, None);
        }

        /// Ensures that pruning in chronological order always succeeds.
        ///
        /// We also check that post pruning is equivalent to never having inserted the node.
        #[test]
        fn pruning_in_fifo_order(
            sequence in (1..100usize)
                .prop_flat_map(|n| UpdateSequence::strategy(n, 0.66))
        ) {
            let uut = sequence.clone().finalize();
            let mut uut = Some(uut);

            let mut deltas = sequence.updates.as_slice();
            while let Some((first, tail)) = deltas.split_first() {
                let reference = UpdateSequence { updates: deltas.to_vec() }.finalize();

                proptest::prop_assert_eq!(&uut, &Some(reference));

                uut = uut.expect("should still be some").prune(first.id);
                deltas = tail;
            }

            // We should have pruned all deltas.
            proptest::prop_assert_eq!(uut, None);
        }
    }
}
