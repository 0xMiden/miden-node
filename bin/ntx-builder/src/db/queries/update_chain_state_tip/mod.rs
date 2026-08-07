//! Updates the tip columns of the singleton chain state row.

use miden_node_db::sqlite::WriteTx;
use miden_node_db::{DatabaseError, SqlTypeConvert};
use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::crypto::merkle::mmr::PartialMmr;

const SQL: &str = include_str!("update_chain_state_tip.sql");

/// Updates the tip columns (block number, header, and partial chain MMR) of the singleton chain
/// state row. The row is created once at bootstrap by
/// [`insert_genesis_chain_state`](super::insert_genesis_chain_state), so this is a plain update;
/// the `genesis_commitment` column is set at bootstrap and never touched here.
pub fn update_chain_state_tip(
    tx: &WriteTx<'_>,
    block_num: BlockNumber,
    block_header: &BlockHeader,
    chain_mmr: &PartialMmr,
) -> Result<(), DatabaseError> {
    tx.execute(SQL, &[&block_num.to_raw_sql(), block_header, chain_mmr])?;
    Ok(())
}
