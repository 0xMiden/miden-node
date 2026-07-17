//! Queries for persisting and retrieving note scripts.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::{ReadTx, WriteTx};
use miden_protocol::Word;
use miden_protocol::note::NoteScript;

use crate::db::sql;

/// Looks up a note script by its root hash.
pub fn lookup_note_script(
    tx: &ReadTx<'_>,
    script_root: &Word,
) -> Result<Option<NoteScript>, DatabaseError> {
    Ok(tx
        .query(sql::LOOKUP_NOTE_SCRIPT, &[script_root], |row| row.get::<NoteScript>(0))?
        .first()
        .cloned())
}

/// Inserts a note script (idempotent via INSERT OR IGNORE).
pub fn insert_note_script(
    tx: &WriteTx<'_>,
    script_root: &Word,
    script: &NoteScript,
) -> Result<(), DatabaseError> {
    tx.execute(sql::INSERT_NOTE_SCRIPT, &[script_root, script])?;
    Ok(())
}
