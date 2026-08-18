//! Persists a block header that this validator has signed.

use miden_node_db::sqlite::WriteTx;
use miden_node_db::{DatabaseError, SqlTypeConvert};
use miden_protocol::block::BlockHeader;

const SQL: &str = include_str!("upsert_block_header.sql");

/// Upserts a block header into the database.
///
/// Inserts a new row if no block header exists at the given block number, or replaces the existing
/// block header if one already exists.
pub fn upsert_block_header(tx: &WriteTx<'_>, header: &BlockHeader) -> Result<(), DatabaseError> {
    tx.execute(SQL, &[&header.block_num().to_raw_sql(), header])?;
    Ok(())
}
