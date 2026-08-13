//! Maps nullifiers to the ids of the notes they consume.

use std::collections::BTreeMap;

use miden_node_db::sqlite::{InList, ReadTx};
use miden_protocol::Word;
use miden_protocol::note::{NoteId, Nullifier};
use miden_protocol::utils::serde::Serializable;

use crate::errors::DatabaseError;

const SQL: &str = include_str!("select_note_ids_by_nullifier.sql");

/// Maps each given nullifier to its note ID.
///
/// Only public notes have a nullifier stored (`notes.nullifier` is NULL for private notes), so
/// private notes never match and are absent from the result.
pub(crate) fn select_note_ids_by_nullifier(
    tx: &ReadTx<'_>,
    nullifiers: &[Nullifier],
) -> Result<BTreeMap<Nullifier, NoteId>, DatabaseError> {
    if nullifiers.is_empty() {
        return Ok(BTreeMap::new());
    }

    let nullifier_bytes = Vec::from_iter(nullifiers.iter().map(Serializable::to_bytes));
    let nullifier_bytes = InList::from_blobs(nullifier_bytes.iter().map(Vec::as_slice));

    let pairs = tx.query(SQL, &[&nullifier_bytes], |row| {
        Ok((row.get::<Option<Nullifier>>(0)?, NoteId::from_raw(row.get::<Word>(1)?)))
    })?;

    Ok(pairs
        .into_iter()
        .filter_map(|(nullifier, note_id)| nullifier.map(|nullifier| (nullifier, note_id)))
        .collect())
}
