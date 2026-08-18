//! Returns the network accounts that currently have pending notes.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::ReadTx;
use miden_protocol::account::AccountId;

const SQL: &str = include_str!("accounts_with_pending_notes.sql");

/// Returns the distinct set of network accounts that currently have at least one pending note
/// (unconsumed and within the per-note attempt budget).
#[expect(clippy::cast_possible_wrap)]
pub fn accounts_with_pending_notes(
    tx: &ReadTx<'_>,
    max_attempts: usize,
) -> Result<Vec<AccountId>, DatabaseError> {
    tx.query(SQL, &[&(max_attempts as i64)], |row| row.get::<AccountId>(0))
}
