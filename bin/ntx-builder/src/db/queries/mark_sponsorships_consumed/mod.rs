//! Marks `FEE_SPONSORSHIP` notes as consumed by the block that contained their nullifier.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::WriteTx;
use miden_protocol::block::BlockNumber;
use miden_protocol::note::Nullifier;

const SQL: &str = include_str!("mark_sponsorship_consumed.sql");

/// Marks `FEE_SPONSORSHIP` notes as consumed by setting `committed_at` to the block number whose
/// committed body contained their nullifier. This covers both consumption alongside the feature
/// note and an external reclaim; either way the note is spent and no longer attachable. Nullifiers
/// we never inserted are silently skipped, so the caller passes the block's full nullifier list.
pub fn mark_sponsorships_consumed(
    tx: &WriteTx<'_>,
    nullifiers: &[Nullifier],
    block_num: BlockNumber,
) -> Result<(), DatabaseError> {
    for nullifier in nullifiers {
        tx.execute(SQL, &[nullifier, &block_num])?;
    }
    Ok(())
}
