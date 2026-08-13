//! Row mapping shared by the `block_headers` queries.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::Row;
use miden_protocol::Word;
use miden_protocol::block::BlockHeader;

use crate::db::BlockHeaderCommitment;

/// Maps a `SELECT block_header, commitment` row to its [`BlockHeader`].
///
/// The stored commitment is only read to assert, in debug builds, that it matches the header it was
/// stored alongside: we are bust if that invariant does not hold.
pub(super) fn block_header_from_row(row: &Row<'_>) -> Result<BlockHeader, DatabaseError> {
    let block_header = row.get::<BlockHeader>(0)?;
    debug_assert_eq!(
        BlockHeaderCommitment::new(&block_header),
        BlockHeaderCommitment(row.get::<Word>(1)?),
        "stored block header commitment disagrees with the stored header",
    );
    Ok(block_header)
}
