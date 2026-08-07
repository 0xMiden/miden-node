//! Checks whether a transaction has already been validated.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::ReadTx;
use miden_protocol::transaction::TransactionId;

const SQL: &str = include_str!("transaction_exists.sql");

/// Returns whether a transaction with the given id has already been validated.
pub fn transaction_exists(tx: &ReadTx<'_>, tx_id: TransactionId) -> Result<bool, DatabaseError> {
    Ok(tx
        .query(SQL, &[&tx_id], |row| row.get::<bool>(0))?
        .into_iter()
        .next()
        .unwrap_or(false))
}
