//! General read queries over the store state.

use std::collections::HashSet;

use miden_node_utils::tracing::miden_instrument;
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::crypto::merkle::mmr::MmrProof;
use miden_protocol::note::{NoteId, NoteScript};

use crate::COMPONENT;
use crate::db::NoteRecord;
use crate::errors::{DatabaseError, GetBlockHeaderError};
use crate::state::{Finality, State, StateView};

impl StateView {
    /// Queries a [BlockHeader] from the database, and returns it alongside its inclusion proof.
    ///
    /// If [None] is given as the value of `block_num`, the data for the latest [BlockHeader] is
    /// returned.
    #[miden_instrument(
        level = "debug",
        target = COMPONENT,
        skip_all,
        err,
    )]
    pub async fn get_block_header(
        &self,
        block_num: Option<BlockNumber>,
        include_mmr_proof: bool,
    ) -> Result<(Option<BlockHeader>, Option<MmrProof>), GetBlockHeaderError> {
        // Resolve "latest" against the view's snapshot rather than the DB: mid-apply, the DB may
        // already contain a block that the snapshot's blockchain cannot prove yet. Scoping the DB
        // query by the view's tip keeps the header and MMR proof consistent.
        let latest_block_num = self.tip();
        let block_num = block_num.unwrap_or(latest_block_num);
        if block_num > latest_block_num {
            return Ok((None, None));
        }

        let block_header = self.db().select_block_header_by_block_num(Some(block_num)).await?;
        if let Some(header) = block_header {
            let mmr_proof = if include_mmr_proof {
                let mmr_proof = self.blockchain().open(header.block_num())?;
                Some(mmr_proof)
            } else {
                None
            };
            Ok((Some(header), mmr_proof))
        } else {
            Ok((None, None))
        }
    }

    /// Queries a list of notes from the database.
    ///
    /// If the provided list of [`NoteId`] given is empty or no note matches the provided
    /// [`NoteId`] an empty list is returned.
    pub async fn get_notes_by_id(
        &self,
        note_ids: Vec<NoteId>,
    ) -> Result<Vec<NoteRecord>, DatabaseError> {
        self.db().select_notes_by_id(note_ids).await
    }

    /// Returns the script for a note by its root.
    pub async fn get_note_script_by_root(
        &self,
        root: Word,
    ) -> Result<Option<NoteScript>, DatabaseError> {
        self.db().select_note_script_by_root(root).await
    }

    /// Filters `account_ids` down to the subset classified as network accounts.
    pub async fn filter_network_accounts(
        &self,
        account_ids: &[AccountId],
    ) -> Result<HashSet<AccountId>, DatabaseError> {
        self.db().select_network_accounts_subset(account_ids.to_vec()).await
    }
}

impl State {
    /// Returns the effective chain tip for the given finality level.
    ///
    /// Both tips are read from the watch channels their writers publish to (the block writer for
    /// [`Finality::Committed`], the proof scheduler or proof sync for [`Finality::Proven`]), so
    /// this is a live value: it advances independently of any [`StateView`](super::StateView).
    /// Reads that must be consistent with data must use a view's tip instead.
    ///
    /// The committed tip is published after the corresponding state snapshot, so it never reports
    /// a block that a freshly created view cannot serve.
    pub fn chain_tip(&self, finality: Finality) -> BlockNumber {
        match finality {
            Finality::Committed => *self.committed_tip_tx.borrow(),
            Finality::Proven => self.proven_tip.read(),
        }
    }
}
