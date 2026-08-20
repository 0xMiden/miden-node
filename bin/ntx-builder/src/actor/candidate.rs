use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use miden_protocol::account::Account;
use miden_protocol::block::BlockHeader;
use miden_protocol::note::{Note, NoteId, Nullifier};
use miden_protocol::transaction::PartialBlockchain;
use miden_standards::note::AccountTargetNetworkNote;

// NOTE GROUP
// ================================================================================================

/// A feature note grouped with the `FEE_SPONSORSHIP` notes that pay its fee.
///
/// The group is the atomic unit of transaction selection: a sponsorship note may only be consumed
/// in the same transaction as its feature note, so a group is included in (or excluded from) a
/// candidate as a whole. A group with no sponsorships is a plain network note.
#[derive(Clone, Debug)]
pub struct NoteGroup {
    /// The network note targeted at the account.
    pub feature: AccountTargetNetworkNote,
    /// `FEE_SPONSORSHIP` notes bound to the feature note, consumed in the same transaction.
    pub sponsorships: Vec<Note>,
}

impl NoteGroup {
    /// Number of notes the group contributes to a transaction: the feature note plus its
    /// sponsorships.
    pub fn num_notes(&self) -> usize {
        1 + self.sponsorships.len()
    }
}

// TRANSACTION CANDIDATE
// ================================================================================================

/// A candidate network transaction.
///
/// Contains the data pertaining to a specific network account which can be used to build a network
/// transaction.
#[derive(Clone, Debug)]
pub struct TransactionCandidate {
    /// The current inflight state of the account.
    ///
    /// Wrapped in `Arc` so building a candidate shares the actor's resident account instead of
    /// deep-cloning it (which, for accounts with large storage maps, is expensive). The account is
    /// only ever read during execution; the actor advances its own copy via `Arc::make_mut` once
    /// the candidate has been consumed.
    pub account: Arc<Account>,

    /// The note groups selected for this transaction: each feature note addressed to the account
    /// together with the sponsorships that pay its fee.
    pub notes: Vec<NoteGroup>,

    /// The latest locally committed block header.
    ///
    /// This should be used as the reference block during transaction execution.
    pub chain_tip_header: BlockHeader,

    /// The chain MMR, which lags behind the tip by one block.
    ///
    /// Wrapped in `Arc` to avoid expensive clones when reading the chain state.
    pub chain_mmr: Arc<PartialBlockchain>,
}

impl TransactionCandidate {
    /// Total number of notes across all groups.
    pub fn num_notes(&self) -> usize {
        self.notes.iter().map(NoteGroup::num_notes).sum()
    }

    /// Maps each sponsorship note id to the nullifier of the feature note it sponsors.
    ///
    /// Sponsorship notes have no row in the `notes` table, so any failure of a sponsorship is
    /// attributed to (and penalizes) the feature note of its group. Feature notes are absent from
    /// the map: their failures are recorded under their own nullifier.
    pub fn sponsor_to_feature_nullifier(&self) -> HashMap<NoteId, Nullifier> {
        self.notes
            .iter()
            .flat_map(|group| {
                let feature_nullifier = group.feature.as_note().nullifier();
                group
                    .sponsorships
                    .iter()
                    .map(move |sponsorship| (sponsorship.id(), feature_nullifier))
            })
            .collect()
    }
}

// GROUP INDEX
// ================================================================================================

/// Pairing metadata derived from a candidate's groups, used to re-pair the note checker's output.
///
/// The consumability checker eliminates notes individually, so it may split a group: keep a
/// sponsorship whose feature note it dropped, or keep a feature note whose sponsorships it
/// dropped. Both halves are guaranteed to fail on-chain (the sponsorship script aborts without its
/// feature note; a fee-charging account's auth procedure rejects an unsponsored feature note), so
/// [`GroupIndex::repair`] removes them before execution.
pub struct GroupIndex {
    /// Sponsorship note id to the id of the feature note it sponsors.
    sponsor_to_feature: HashMap<NoteId, NoteId>,
    /// Feature notes that must not execute without at least one of their sponsorships.
    gated_features: HashSet<NoteId>,
}

impl GroupIndex {
    /// Builds the index from a candidate's groups.
    ///
    /// `require_sponsorship` mirrors the selection-time gate: when set, a feature note that was
    /// selected together with sponsorships must not execute after losing all of them.
    pub fn new(groups: &[NoteGroup], require_sponsorship: bool) -> Self {
        let sponsor_to_feature = groups
            .iter()
            .flat_map(|group| {
                let feature_id = group.feature.as_note().id();
                group.sponsorships.iter().map(move |sponsorship| (sponsorship.id(), feature_id))
            })
            .collect();
        let gated_features = if require_sponsorship {
            groups
                .iter()
                .filter(|group| !group.sponsorships.is_empty())
                .map(|group| group.feature.as_note().id())
                .collect()
        } else {
            HashSet::new()
        };
        Self { sponsor_to_feature, gated_features }
    }

    /// Splits the checker's surviving notes into `(retained, dropped)`, removing notes that must
    /// not execute after the checker eliminated part of their group: sponsorships whose feature
    /// note is gone, and gated feature notes that lost every sponsorship.
    pub fn repair(&self, notes: Vec<Note>) -> (Vec<Note>, Vec<Note>) {
        let ids: HashSet<NoteId> = notes.iter().map(Note::id).collect();
        // A sponsorship survives when its feature note also survived the checker; the features
        // named here satisfy the gate.
        let sponsored_features: HashSet<NoteId> = self
            .sponsor_to_feature
            .iter()
            .filter(|(sponsorship, feature)| ids.contains(sponsorship) && ids.contains(feature))
            .map(|(_, feature)| *feature)
            .collect();

        notes.into_iter().partition(|note| {
            if let Some(feature) = self.sponsor_to_feature.get(&note.id()) {
                ids.contains(feature)
            } else if self.gated_features.contains(&note.id()) {
                sponsored_features.contains(&note.id())
            } else {
                true
            }
        })
    }
}

// TESTS
// ================================================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{
        mock_network_account_id,
        mock_single_target_note,
        mock_sponsorship_note,
    };

    /// Builds a group of one feature note and `num_sponsorships` sponsorships bound to it.
    fn group(feature_seed: u8, num_sponsorships: u8) -> NoteGroup {
        let account_id = mock_network_account_id();
        let feature = mock_single_target_note(account_id, feature_seed);
        let sponsorships = (0..num_sponsorships)
            .map(|i| {
                mock_sponsorship_note(account_id, feature.as_note().id(), feature_seed + 100 + i)
            })
            .collect();
        NoteGroup { feature, sponsorships }
    }

    fn flatten(groups: &[NoteGroup]) -> Vec<Note> {
        groups
            .iter()
            .flat_map(|g| {
                std::iter::once(g.feature.as_note().clone()).chain(g.sponsorships.iter().cloned())
            })
            .collect()
    }

    /// An intact group passes repair untouched, with or without the gate.
    #[test]
    fn repair_keeps_intact_groups() {
        let groups = [group(1, 2), group(2, 0)];
        let notes = flatten(&groups);

        for require in [false, true] {
            let index = GroupIndex::new(&groups, require);
            let (retained, dropped) = index.repair(notes.clone());
            assert_eq!(retained.len(), 4);
            assert!(dropped.is_empty());
        }
    }

    /// A sponsorship whose feature note the checker eliminated is dropped: its script would abort
    /// the VM.
    #[test]
    fn repair_drops_orphaned_sponsorship() {
        let groups = [group(1, 1), group(2, 0)];
        let index = GroupIndex::new(&groups, false);

        // The checker eliminated the feature note of group 1; its sponsorship survived.
        let notes = vec![groups[0].sponsorships[0].clone(), groups[1].feature.as_note().clone()];
        let (retained, dropped) = index.repair(notes);

        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].id(), groups[1].feature.as_note().id());
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].id(), groups[0].sponsorships[0].id());
    }

    /// With the sponsorship requirement active, a feature note that lost every sponsorship is
    /// dropped as well: the account's auth procedure would reject it.
    #[test]
    fn repair_drops_gated_feature_without_surviving_sponsorship() {
        let groups = [group(1, 1)];
        let notes = vec![groups[0].feature.as_note().clone()];

        let gated = GroupIndex::new(&groups, true);
        let (retained, dropped) = gated.repair(notes.clone());
        assert!(retained.is_empty());
        assert_eq!(dropped.len(), 1);

        // Without the requirement the feature note executes alone (the account may not charge
        // fees).
        let ungated = GroupIndex::new(&groups, false);
        let (retained, dropped) = ungated.repair(notes);
        assert_eq!(retained.len(), 1);
        assert!(dropped.is_empty());
    }

    /// A gated feature keeps executing while at least one of its sponsorships survived.
    #[test]
    fn repair_keeps_gated_feature_with_one_surviving_sponsorship() {
        let groups = [group(1, 2)];
        let index = GroupIndex::new(&groups, true);

        // One of the two sponsorships was eliminated by the checker.
        let notes = vec![groups[0].feature.as_note().clone(), groups[0].sponsorships[1].clone()];
        let (retained, dropped) = index.repair(notes);

        assert_eq!(retained.len(), 2);
        assert!(dropped.is_empty());
    }

    /// The failure-attribution map names every sponsorship and no feature note.
    #[test]
    fn sponsor_to_feature_nullifier_covers_sponsorships_only() {
        let groups = [group(1, 2), group(2, 0)];
        let chain_mmr = PartialBlockchain::new(
            miden_protocol::crypto::merkle::mmr::PartialMmr::from_peaks(
                miden_protocol::crypto::merkle::mmr::MmrPeaks::new(
                    miden_protocol::crypto::merkle::mmr::Forest::new(0).unwrap(),
                    vec![],
                )
                .unwrap(),
            ),
            [],
        )
        .unwrap();
        let candidate = TransactionCandidate {
            account: Arc::new(crate::test_utils::mock_account(mock_network_account_id())),
            notes: groups.to_vec(),
            chain_tip_header: crate::test_utils::mock_block_header(0_u32.into()),
            chain_mmr: Arc::new(chain_mmr),
        };

        let map = candidate.sponsor_to_feature_nullifier();
        assert_eq!(map.len(), 2);
        let feature_nullifier = groups[0].feature.as_note().nullifier();
        for sponsorship in &groups[0].sponsorships {
            assert_eq!(map[&sponsorship.id()], feature_nullifier);
        }
        assert_eq!(candidate.num_notes(), 4);
    }
}
