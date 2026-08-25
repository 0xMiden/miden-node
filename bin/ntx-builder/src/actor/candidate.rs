use std::collections::HashMap;
use std::sync::Arc;

use miden_protocol::Word;
use miden_protocol::account::Account;
use miden_protocol::asset::Asset;
use miden_protocol::block::BlockHeader;
use miden_protocol::note::{Note, NoteId, Nullifier};
use miden_protocol::transaction::PartialBlockchain;
use miden_standards::note::AccountTargetNetworkNote;

// NOTE GROUP
// ================================================================================================

/// A feature note grouped with the `FEE_SPONSORSHIP` notes that pay its fee.
///
/// Transaction selection packs a group as a unit because a sponsorship note may only be consumed
/// with its feature note. Consumability filtering may retain a valid subset: a feature can execute
/// without every selected sponsorship when its required fee is otherwise covered. A group with no
/// sponsorships is a plain network note.
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

    /// Retains only sponsorships carrying the fee asset accepted by the network account.
    ///
    /// This must run before applying the per-feature sponsorship cap so notes carrying an
    /// unrelated asset cannot crowd valid sponsorships out of the candidate.
    pub fn retain_sponsorships_for_fee_asset(&mut self, fee_asset_id: Word) {
        self.sponsorships.retain(|note| {
            note.assets()
                .as_slice()
                .first()
                .is_some_and(|asset| asset.id().to_word() == fee_asset_id)
        });
    }

    /// Sorts sponsorships by descending fungible amount, leaving malformed non-fungible
    /// sponsorships last.
    pub fn sort_sponsorships_by_amount(&mut self) {
        self.sponsorships.sort_by(|left, right| {
            sponsorship_amount(right)
                .cmp(&sponsorship_amount(left))
                .then_with(|| left.id().cmp(&right.id()))
        });
    }
}

/// Returns the fungible amount carried by a sponsorship, or zero for a malformed non-fungible
/// sponsorship. Sponsorship ingestion is responsible for rejecting the latter.
pub(super) fn sponsorship_amount(note: &Note) -> u64 {
    match note.assets().as_slice().first() {
        Some(Asset::Fungible(asset)) => asset.amount().as_u64(),
        Some(Asset::NonFungible(_)) | None => 0,
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
