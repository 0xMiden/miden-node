//! Reads the genesis block commitment from the singleton chain state row.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::ReadTx;
use miden_protocol::Word;

const SQL: &str = include_str!("select_genesis_commitment.sql");

/// Reads the genesis block commitment from the singleton chain state row, or `None` if the database
/// has not been bootstrapped.
pub fn select_genesis_commitment(tx: &ReadTx<'_>) -> Result<Option<Word>, DatabaseError> {
    Ok(tx.query(SQL, &[], |row| row.get::<Word>(0))?.into_iter().next())
}
