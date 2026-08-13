//! Returns which of the given note commitments are already stored.

use std::collections::HashSet;

use miden_node_db::sqlite::{InList, ReadTx};
use miden_node_utils::limiter::{QueryParamLimiter, QueryParamNoteCommitmentLimit};
use miden_protocol::Word;
use miden_protocol::block::BlockNumber;
use miden_protocol::utils::serde::Serializable;

use crate::errors::DatabaseError;

const SQL: &str = include_str!("select_existing_note_commitments.sql");

/// Select the subset of note commitments that already exist in the notes table and were committed
/// at or before `up_to_block`.
pub(crate) fn select_existing_note_commitments(
    tx: &ReadTx<'_>,
    note_commitments: &[Word],
    up_to_block: BlockNumber,
) -> Result<HashSet<Word>, DatabaseError> {
    QueryParamNoteCommitmentLimit::check(note_commitments.len())?;

    let commitments = Vec::from_iter(note_commitments.iter().map(Serializable::to_bytes));
    let commitments = InList::from_blobs(commitments.iter().map(Vec::as_slice));

    Ok(tx
        .query(SQL, &[&commitments, &up_to_block], |row| row.get::<Word>(0))?
        .into_iter()
        .collect())
}
