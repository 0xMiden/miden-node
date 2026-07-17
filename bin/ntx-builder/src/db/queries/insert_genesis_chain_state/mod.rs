//! Seeds the singleton chain state row at bootstrap.

use miden_node_db::sqlite::WriteTx;
use miden_node_db::{DatabaseError, SqlTypeConvert};
use miden_protocol::Word;
use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::crypto::merkle::mmr::PartialMmr;

const SQL: &str = include_str!("insert_genesis_chain_state.sql");

/// Inserts the singleton chain state row at bootstrap, seeding the tip columns from the genesis
/// block together with the genesis block commitment. The commitment satisfies the `NOT NULL`
/// constraint at insert time and is retained across all subsequent tip updates (see
/// [`update_chain_state_tip`](super::update_chain_state_tip)).
pub fn insert_genesis_chain_state(
    tx: &WriteTx<'_>,
    genesis_block_header: &BlockHeader,
    genesis_commitment: &Word,
) -> Result<(), DatabaseError> {
    assert_eq!(
        genesis_block_header.block_num(),
        BlockNumber::GENESIS,
        "bootstrap block number is not 0"
    );
    tx.execute(
        SQL,
        &[
            &genesis_block_header.block_num().to_raw_sql(),
            genesis_block_header,
            &PartialMmr::default(),
            genesis_commitment,
        ],
    )?;
    Ok(())
}
