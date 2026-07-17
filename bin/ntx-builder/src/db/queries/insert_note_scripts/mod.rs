//! Inserts a note script (idempotent via INSERT OR IGNORE).

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::WriteTx;
use miden_protocol::Word;
use miden_protocol::note::NoteScript;

const SQL: &str = include_str!("insert_note_script.sql");

/// Inserts a note script (idempotent via INSERT OR IGNORE).
pub fn insert_note_script(
    tx: &WriteTx<'_>,
    script_root: &Word,
    script: &NoteScript,
) -> Result<(), DatabaseError> {
    tx.execute(SQL, &[script_root, script])?;
    Ok(())
}
