//! Returns a single block header, either at a given block number or at the chain tip.

use miden_node_db::sqlite::ReadTx;
use miden_protocol::block::{BlockHeader, BlockNumber};

use crate::db::queries::block_header_row::block_header_from_row;
use crate::errors::DatabaseError;

const SQL_BY_BLOCK_NUM: &str = include_str!("select_block_header_by_block_num.sql");
const SQL_LATEST: &str = include_str!("select_latest_block_header.sql");

/// Returns the block header at `maybe_block_num`, or the latest block header when it is `None`.
///
/// The two cases are separate statements rather than one statement with a nullable parameter, so the
/// lookup by block number stays an equality match on the primary key instead of an ordered scan.
pub(crate) fn select_block_header_by_block_num(
    tx: &ReadTx<'_>,
    maybe_block_num: Option<BlockNumber>,
) -> Result<Option<BlockHeader>, DatabaseError> {
    // Invariant: `block_num` is the primary key, so either statement returns at most one row.
    let rows = match maybe_block_num {
        Some(block_num) => tx.query(SQL_BY_BLOCK_NUM, &[&block_num], block_header_from_row)?,
        None => tx.query(SQL_LATEST, &[], block_header_from_row)?,
    };

    Ok(rows.into_iter().next())
}
