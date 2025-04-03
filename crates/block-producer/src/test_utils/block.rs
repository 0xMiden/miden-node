use miden_objects::{
    ACCOUNT_TREE_DEPTH, Digest,
    batch::ProvenBatch,
    block::{
        BlockAccountUpdate, BlockHeader, BlockNoteIndex, BlockNoteTree, OutputNoteBatch,
        ProvenBlock,
    },
    crypto::merkle::{Mmr, SimpleSmt},
    note::Nullifier,
    transaction::{OrderedTransactionHeaders, OutputNote},
};

use super::MockStoreSuccess;

/// Constructs the block we expect to be built given the store state, and a set of transaction
/// batches to be applied
pub async fn build_expected_block_header(
    store: &MockStoreSuccess,
    batches: &[ProvenBatch],
) -> BlockHeader {
    let last_block_header = store
        .block_headers
        .read()
        .await
        .iter()
        .max_by_key(|(block_num, _)| *block_num)
        .unwrap()
        .1
        .clone();

    // Compute new account root
    let updated_accounts: Vec<_> =
        batches.iter().flat_map(|batch| batch.account_updates().iter()).collect();
    let new_account_root = {
        let mut store_accounts = store.accounts.read().await.clone();
        for (&account_id, update) in updated_accounts {
            store_accounts.insert(account_id.into(), update.final_state_commitment().into());
        }

        store_accounts.root()
    };

    // Compute new chain MMR root
    let new_chain_mmr_root = {
        let mut store_chain_mmr = store.chain_mmr.read().await.clone();

        store_chain_mmr.add(last_block_header.commitment());

        store_chain_mmr.peaks().hash_peaks()
    };

    let note_created_smt =
        note_created_smt_from_note_batches(block_output_notes(batches.iter()).iter());

    // Build header
    BlockHeader::new(
        0,
        last_block_header.commitment(),
        last_block_header.block_num() + 1,
        new_chain_mmr_root,
        new_account_root,
        // FIXME: FILL IN CORRECT NULLIFIER ROOT
        Digest::default(),
        note_created_smt.root(),
        Digest::default(),
        Digest::default(),
        Digest::default(),
        1,
    )
}

#[derive(Debug)]
pub struct MockBlockBuilder {
    store_accounts: SimpleSmt<ACCOUNT_TREE_DEPTH>,
    store_chain_mmr: Mmr,
    last_block_header: BlockHeader,

    updated_accounts: Option<Vec<BlockAccountUpdate>>,
    created_notes: Option<Vec<OutputNoteBatch>>,
    produced_nullifiers: Option<Vec<Nullifier>>,
}

impl MockBlockBuilder {
    pub async fn new(store: &MockStoreSuccess) -> Self {
        Self {
            store_accounts: store.accounts.read().await.clone(),
            store_chain_mmr: store.chain_mmr.read().await.clone(),
            last_block_header: store
                .block_headers
                .read()
                .await
                .iter()
                .max_by_key(|(block_num, _)| *block_num)
                .unwrap()
                .1
                .clone(),

            updated_accounts: None,
            created_notes: None,
            produced_nullifiers: None,
        }
    }

    #[must_use]
    pub fn account_updates(mut self, updated_accounts: Vec<BlockAccountUpdate>) -> Self {
        for update in &updated_accounts {
            self.store_accounts
                .insert(update.account_id().into(), update.final_state_commitment().into());
        }

        self.updated_accounts = Some(updated_accounts);

        self
    }

    #[must_use]
    pub fn created_notes(mut self, created_notes: Vec<OutputNoteBatch>) -> Self {
        self.created_notes = Some(created_notes);

        self
    }

    #[must_use]
    pub fn produced_nullifiers(mut self, produced_nullifiers: Vec<Nullifier>) -> Self {
        self.produced_nullifiers = Some(produced_nullifiers);

        self
    }

    pub fn build(self) -> ProvenBlock {
        let created_notes = self.created_notes.unwrap_or_default();

        let header = BlockHeader::new(
            0,
            self.last_block_header.commitment(),
            self.last_block_header.block_num() + 1,
            self.store_chain_mmr.peaks().hash_peaks(),
            self.store_accounts.root(),
            Digest::default(),
            note_created_smt_from_note_batches(created_notes.iter()).root(),
            Digest::default(),
            Digest::default(),
            Digest::default(),
            1,
        );

        ProvenBlock::new_unchecked(
            header,
            self.updated_accounts.unwrap_or_default(),
            created_notes,
            self.produced_nullifiers.unwrap_or_default(),
            OrderedTransactionHeaders::new_unchecked(vec![]),
        )
    }
}

pub(crate) fn flatten_output_notes<'a>(
    batches: impl Iterator<Item = &'a OutputNoteBatch>,
) -> impl Iterator<Item = (BlockNoteIndex, &'a OutputNote)> {
    batches.enumerate().flat_map(|(batch_idx, batch)| {
        batch.iter().map(move |(note_idx_in_batch, note)| {
            (BlockNoteIndex::new(batch_idx, *note_idx_in_batch).unwrap(), note)
        })
    })
}

pub(crate) fn note_created_smt_from_note_batches<'a>(
    batches: impl Iterator<Item = &'a OutputNoteBatch>,
) -> BlockNoteTree {
    let note_leaf_iterator =
        flatten_output_notes(batches).map(|(index, note)| (index, note.id(), *note.metadata()));

    BlockNoteTree::with_entries(note_leaf_iterator).unwrap()
}

pub(crate) fn block_output_notes<'a>(
    batches: impl Iterator<Item = &'a ProvenBatch> + Clone,
) -> Vec<OutputNoteBatch> {
    batches
        .map(|batch| batch.output_notes().iter().cloned().enumerate().collect())
        .collect()
}
