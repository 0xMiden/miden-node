//! Returns the block headers for a set of block numbers.

use miden_node_db::SqlTypeConvert;
use miden_node_db::sqlite::{InList, ReadTx};
use miden_node_utils::limiter::{QueryParamBlockLimit, QueryParamLimiter};
use miden_protocol::block::{BlockHeader, BlockNumber};

use crate::db::queries::block_header_row::block_header_from_row;
use crate::errors::DatabaseError;

const SQL: &str = include_str!("select_block_headers.sql");

/// Returns the block headers stored at `blocks`, ordered by block number.
///
/// Block numbers without a stored header are skipped, so the result may be shorter than `blocks`.
///
/// # Parameters
///
/// * `blocks`: the block numbers to retrieve, at most [`QueryParamBlockLimit`] of them.
pub(crate) fn select_block_headers(
    tx: &ReadTx<'_>,
    blocks: impl Iterator<Item = BlockNumber> + Send,
) -> Result<Vec<BlockHeader>, DatabaseError> {
    // The iterators are all deterministic, so is the conjunction.
    // All calling sites do it equivalently, hence the below holds.
    // <https://doc.rust-lang.org/src/core/slice/iter/macros.rs.html#195>
    // <https://doc.rust-lang.org/src/core/option.rs.html#2273>
    // And the conjunction is truthful:
    // <https://doc.rust-lang.org/src/core/iter/adapters/chain.rs.html#184>
    QueryParamBlockLimit::check(blocks.size_hint().0)?;

    let blocks = InList::from_i64s(blocks.map(SqlTypeConvert::to_raw_sql));

    Ok(tx.query(SQL, &[&blocks], block_header_from_row)?)
}
