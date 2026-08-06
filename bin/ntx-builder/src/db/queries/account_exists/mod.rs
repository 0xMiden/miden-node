//! Checks whether a network account is tracked locally.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::ReadTx;
use miden_protocol::account::AccountId;

const SQL: &str = include_str!("account_exists.sql");

/// Returns `true` if a committed state for the given account is tracked locally.
pub fn account_exists(tx: &ReadTx<'_>, account_id: AccountId) -> Result<bool, DatabaseError> {
    Ok(tx
        .query(SQL, &[&account_id], |row| row.get::<bool>(0))?
        .into_iter()
        .next()
        .unwrap_or(false))
}
