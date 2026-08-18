//! Counts the blocks this validator has signed.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::ReadTx;

const SQL: &str = include_str!("count_signed_blocks.sql");

/// Returns the total number of signed blocks in the database.
pub fn count_signed_blocks(tx: &ReadTx<'_>) -> Result<i64, DatabaseError> {
    Ok(tx
        .query(SQL, &[], |row| row.get::<i64>(0))?
        .into_iter()
        .next()
        .unwrap_or(0))
}
