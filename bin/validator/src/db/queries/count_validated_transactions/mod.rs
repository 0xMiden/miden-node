//! Counts the validated transactions recorded so far.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::ReadTx;

const SQL: &str = include_str!("count_validated_transactions.sql");

/// Returns the total number of validated transactions in the database.
pub fn count_validated_transactions(tx: &ReadTx<'_>) -> Result<i64, DatabaseError> {
    Ok(tx.query(SQL, &[], |row| row.get::<i64>(0))?.into_iter().next().unwrap_or(0))
}
