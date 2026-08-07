//! Reads the block header stored at a given height.

use miden_node_db::sqlite::ReadTx;
use miden_node_db::{DatabaseError, SqlTypeConvert};
use miden_protocol::block::{BlockHeader, BlockNumber};

const SQL: &str = include_str!("load_block_header.sql");

/// Loads a block header by its block number.
///
/// Returns `None` if no block header exists at the given block number.
pub fn load_block_header(
    tx: &ReadTx<'_>,
    block_num: BlockNumber,
) -> Result<Option<BlockHeader>, DatabaseError> {
    Ok(tx
        .query(SQL, &[&block_num.to_raw_sql()], |row| row.get::<BlockHeader>(0))?
        .into_iter()
        .next())
}
