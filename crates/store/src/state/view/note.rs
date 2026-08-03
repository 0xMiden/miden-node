//! Note reads.
//!
//! These are content-addressed lookups and technically not block-scoped, but they live on
//! [`StateView`] so that every read path flows through the same type.

use miden_protocol::Word;
use miden_protocol::note::{NoteId, NoteScript};

use super::StateView;
use crate::db::NoteRecord;
use crate::errors::DatabaseError;

impl StateView {
    /// Queries a list of notes from the database.
    ///
    /// If the provided list of [`NoteId`]s is empty or no note matches, an empty list is
    /// returned. This lookup is deliberately not bounded by this view's tip (latest-wins): a note
    /// committed while a block is being applied may be returned before the snapshot advances.
    pub async fn get_notes_by_id(
        &self,
        note_ids: Vec<NoteId>,
    ) -> Result<Vec<NoteRecord>, DatabaseError> {
        self.db.select_notes_by_id(note_ids).await
    }

    /// Returns the script for a note by its root.
    pub async fn get_note_script_by_root(
        &self,
        root: Word,
    ) -> Result<Option<NoteScript>, DatabaseError> {
        self.db.select_note_script_by_root(root).await
    }
}
