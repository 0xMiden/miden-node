//! Looks up a note script by its root hash.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::ReadTx;
use miden_protocol::Word;
use miden_protocol::note::NoteScript;

const SQL: &str = include_str!("lookup_note_script.sql");

/// Looks up a note script by its root hash.
pub fn lookup_note_script(
    tx: &ReadTx<'_>,
    script_root: &Word,
) -> Result<Option<NoteScript>, DatabaseError> {
    Ok(tx.query(SQL, &[script_root], |row| row.get::<NoteScript>(0))?.first().cloned())
}
