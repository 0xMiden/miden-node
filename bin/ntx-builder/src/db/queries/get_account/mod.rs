//! Returns the committed account state for a given network account.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::ReadTx;
use miden_protocol::account::{Account, AccountId};

const SQL: &str = include_str!("get_account.sql");

/// Returns the committed account state for the given network account.
pub fn get_account(
    tx: &ReadTx<'_>,
    account_id: AccountId,
) -> Result<Option<Account>, DatabaseError> {
    Ok(tx.query(SQL, &[&account_id], |row| row.get::<Account>(0))?.into_iter().next())
}
