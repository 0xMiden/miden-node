//! Cheap existence check for an account's pending notes.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::ReadTx;
use miden_protocol::account::AccountId;

const SQL: &str = include_str!("account_has_pending_notes.sql");

/// Returns `true` if the account has any pending note: unconsumed and within the per-note attempt
/// budget. This is the cheap equivalent of "does [`available_notes`](super::available_notes) return
/// a note that is eligible or awaiting a retry window" (every row passing this filter is one or the
/// other), but it tests for existence in SQL and deserializes nothing. The coordinator uses it to
/// decide whether to respawn an actor that just idle-timed-out.
#[expect(clippy::cast_possible_wrap)]
pub fn account_has_pending_notes(
    tx: &ReadTx<'_>,
    account_id: AccountId,
    max_attempts: usize,
) -> Result<bool, DatabaseError> {
    Ok(tx
        .query(SQL, &[&account_id, &(max_attempts as i64)], |row| row.get::<bool>(0))?
        .into_iter()
        .next()
        .unwrap_or(false))
}
