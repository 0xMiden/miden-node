use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU32,
    sync::Arc,
};

use miden_objects::{
    ACCOUNT_TREE_DEPTH, Digest, EMPTY_WORD, ZERO,
    account::AccountId,
    batch::ProvenBatch,
    block::{BlockHeader, BlockNumber, OutputNoteBatch, ProvenBlock},
    crypto::merkle::{Mmr, SimpleSmt, Smt},
    note::{NoteId, NoteInclusionProof},
    transaction::ProvenTransaction,
};
use tokio::sync::RwLock;

use crate::{
    errors::StoreError,
    store::TransactionInputs,
    test_utils::block::{
        block_output_notes, flatten_output_notes, note_created_smt_from_note_batches,
    },
};

/// Builds a [`MockStoreSuccess`]
#[derive(Debug)]
pub struct MockStoreSuccessBuilder {
    accounts: Option<SimpleSmt<ACCOUNT_TREE_DEPTH>>,
    notes: Option<Vec<OutputNoteBatch>>,
    produced_nullifiers: Option<BTreeSet<Digest>>,
    chain_mmr: Option<Mmr>,
    block_num: Option<BlockNumber>,
}

impl MockStoreSuccessBuilder {
    pub fn from_batches<'a>(batches_iter: impl Iterator<Item = &'a ProvenBatch> + Clone) -> Self {
        let accounts_smt = {
            let accounts = batches_iter
                .clone()
                .flat_map(|batch| {
                    batch
                        .account_updates()
                        .iter()
                        .map(|(account_id, update)| (account_id, update.initial_state_commitment()))
                })
                .map(|(account_id, commitment)| (account_id.prefix().into(), commitment.into()));
            SimpleSmt::<ACCOUNT_TREE_DEPTH>::with_leaves(accounts).unwrap()
        };

        Self {
            accounts: Some(accounts_smt),
            notes: Some(block_output_notes(batches_iter)),
            produced_nullifiers: None,
            chain_mmr: None,
            block_num: None,
        }
    }

    pub fn from_accounts(accounts: impl Iterator<Item = (AccountId, Digest)>) -> Self {
        let accounts_smt = {
            let accounts = accounts
                .map(|(account_id, commitment)| (account_id.prefix().into(), commitment.into()));

            SimpleSmt::<ACCOUNT_TREE_DEPTH>::with_leaves(accounts).unwrap()
        };

        Self {
            accounts: Some(accounts_smt),
            notes: None,
            produced_nullifiers: None,
            chain_mmr: None,
            block_num: None,
        }
    }

    #[must_use]
    pub fn initial_notes<'a>(
        mut self,
        notes: impl Iterator<Item = &'a OutputNoteBatch> + Clone,
    ) -> Self {
        self.notes = Some(notes.cloned().collect());

        self
    }

    #[must_use]
    pub fn initial_nullifiers(mut self, nullifiers: BTreeSet<Digest>) -> Self {
        self.produced_nullifiers = Some(nullifiers);

        self
    }

    #[must_use]
    pub fn initial_chain_mmr(mut self, chain_mmr: Mmr) -> Self {
        self.chain_mmr = Some(chain_mmr);

        self
    }

    #[must_use]
    pub fn initial_block_num(mut self, block_num: BlockNumber) -> Self {
        self.block_num = Some(block_num);

        self
    }

    pub fn build(self) -> MockStoreSuccess {
        let block_num = self.block_num.unwrap_or(1.into());
        let accounts_smt = self.accounts.unwrap_or(SimpleSmt::new().unwrap());
        let notes = self.notes.unwrap_or_default();
        let block_note_tree = note_created_smt_from_note_batches(notes.iter());
        let note_root = block_note_tree.root();
        let chain_mmr = self.chain_mmr.unwrap_or_default();
        let nullifiers_smt = self
            .produced_nullifiers
            .map(|nullifiers| {
                Smt::with_entries(
                    nullifiers
                        .into_iter()
                        .map(|nullifier| (nullifier, [block_num.into(), ZERO, ZERO, ZERO])),
                )
                .unwrap()
            })
            .unwrap_or_default();

        let initial_block_header = BlockHeader::new(
            0,
            Digest::default(),
            block_num,
            chain_mmr.peaks().hash_peaks(),
            accounts_smt.root(),
            nullifiers_smt.root(),
            note_root,
            Digest::default(),
            Digest::default(),
            Digest::default(),
            1,
        );

        let notes = flatten_output_notes(notes.iter())
            .map(|(index, note)| {
                (
                    note.id(),
                    NoteInclusionProof::new(
                        block_num,
                        index.leaf_index_value(),
                        block_note_tree.get_note_path(index),
                    )
                    .expect("Failed to create `NoteInclusionProof`"),
                )
            })
            .collect();

        MockStoreSuccess {
            accounts: Arc::new(RwLock::new(accounts_smt)),
            produced_nullifiers: Arc::new(RwLock::new(nullifiers_smt)),
            chain_mmr: Arc::new(RwLock::new(chain_mmr)),
            block_headers: Arc::new(RwLock::new(BTreeMap::from_iter([(
                initial_block_header.block_num(),
                initial_block_header,
            )]))),
            num_apply_block_called: Arc::default(),
            notes: Arc::new(RwLock::new(notes)),
        }
    }
}

pub struct MockStoreSuccess {
    /// Map account id -> account commitment
    pub accounts: Arc<RwLock<SimpleSmt<ACCOUNT_TREE_DEPTH>>>,

    /// Stores the nullifiers of the notes that were consumed
    pub produced_nullifiers: Arc<RwLock<Smt>>,

    /// Stores the chain MMR
    pub chain_mmr: Arc<RwLock<Mmr>>,

    /// The chains block headers.
    pub block_headers: Arc<RwLock<BTreeMap<BlockNumber, BlockHeader>>>,

    /// The number of times `apply_block()` was called
    pub num_apply_block_called: Arc<RwLock<u32>>,

    /// Maps note id -> note inclusion proof for all created notes
    pub notes: Arc<RwLock<BTreeMap<NoteId, NoteInclusionProof>>>,
}

impl MockStoreSuccess {
    pub async fn account_root(&self) -> Digest {
        let locked_accounts = self.accounts.read().await;

        locked_accounts.root()
    }

    pub async fn apply_block(&self, block: &ProvenBlock) -> Result<(), StoreError> {
        // Intentionally, we take and hold both locks, to prevent calls to `get_tx_inputs()` from
        // going through while we're updating the store's data structure
        let mut locked_accounts = self.accounts.write().await;
        let mut locked_produced_nullifiers = self.produced_nullifiers.write().await;

        // update accounts
        for update in block.updated_accounts() {
            locked_accounts
                .insert(update.account_id().into(), update.final_state_commitment().into());
        }
        let header = block.header();
        debug_assert_eq!(locked_accounts.root(), header.account_root());

        // update nullifiers
        for nullifier in block.created_nullifiers() {
            locked_produced_nullifiers
                .insert(nullifier.inner(), [header.block_num().into(), ZERO, ZERO, ZERO]);
        }

        // update chain mmr with new block header hash
        {
            let mut chain_mmr = self.chain_mmr.write().await;

            chain_mmr.add(block.commitment());
        }

        // build note tree
        let note_tree = block.build_output_note_tree();

        // update notes
        let mut locked_notes = self.notes.write().await;
        for (note_index, note) in block.output_notes() {
            locked_notes.insert(
                note.id(),
                NoteInclusionProof::new(
                    header.block_num(),
                    note_index.leaf_index_value(),
                    note_tree.get_note_path(note_index),
                )
                .expect("Failed to build `NoteInclusionProof`"),
            );
        }

        // append the block header
        self.block_headers.write().await.insert(header.block_num(), header.clone());

        // update num_apply_block_called
        *self.num_apply_block_called.write().await += 1;

        Ok(())
    }

    pub async fn get_tx_inputs(
        &self,
        proven_tx: &ProvenTransaction,
    ) -> Result<TransactionInputs, StoreError> {
        let locked_accounts = self.accounts.read().await;
        let locked_produced_nullifiers = self.produced_nullifiers.read().await;

        let account_commitment = {
            let account_commitment = locked_accounts.get_leaf(&proven_tx.account_id().into());

            if account_commitment == EMPTY_WORD {
                None
            } else {
                Some(account_commitment.into())
            }
        };

        let nullifiers = proven_tx
            .input_notes()
            .iter()
            .map(|commitment| {
                let nullifier = commitment.nullifier();
                let nullifier_value = locked_produced_nullifiers.get_value(&nullifier.inner());

                (nullifier, NonZeroU32::new(nullifier_value[0].inner() as u32))
            })
            .collect();

        let locked_notes = self.notes.read().await;
        let found_unauthenticated_notes = proven_tx
            .unauthenticated_notes()
            .filter_map(|header| {
                let id = header.id();
                locked_notes.contains_key(&id).then_some(id)
            })
            .collect();

        Ok(TransactionInputs {
            account_id: proven_tx.account_id(),
            account_commitment,
            nullifiers,
            found_unauthenticated_notes,
            current_block_height: 0.into(),
        })
    }
}
