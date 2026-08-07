//! Returns the latest transaction recorded against a network account.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::ReadTx;
use miden_protocol::account::AccountId;
use miden_protocol::transaction::TransactionId;

const SQL: &str = include_str!("account_last_tx.sql");

/// Returns the latest transaction recorded against `account_id`, or `None` if the account is not
/// tracked locally.
pub fn account_last_tx(
    tx: &ReadTx<'_>,
    account_id: AccountId,
) -> Result<Option<TransactionId>, DatabaseError> {
    Ok(tx
        .query(SQL, &[&account_id], |row| row.get::<TransactionId>(0))?
        .into_iter()
        .next())
}
