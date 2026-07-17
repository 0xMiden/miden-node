//! Chain state queries.

use miden_node_db::sqlite::{ReadTx, WriteTx};
use miden_node_db::{DatabaseError, SqlTypeConvert};
use miden_protocol::Word;
use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::crypto::merkle::mmr::PartialMmr;

use crate::db::sql;

/// Updates the tip columns (block number, header, and partial chain MMR) of the singleton chain
/// state row. The row is created once at bootstrap by [`insert_genesis_chain_state`], so this is a
/// plain update; the `genesis_commitment` column is set at bootstrap and never touched here.
pub fn update_chain_state_tip(
    tx: &WriteTx<'_>,
    block_num: BlockNumber,
    block_header: &BlockHeader,
    chain_mmr: &PartialMmr,
) -> Result<(), DatabaseError> {
    tx.execute(sql::UPDATE_CHAIN_STATE_TIP, &[&block_num.to_raw_sql(), block_header, chain_mmr])?;
    Ok(())
}

/// Inserts the singleton chain state row at bootstrap, seeding the tip columns from the genesis
/// block together with the genesis block commitment. The commitment satisfies the `NOT NULL`
/// constraint at insert time and is retained across all subsequent tip updates (see
/// [`update_chain_state_tip`]).
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
        sql::INSERT_GENESIS_CHAIN_STATE,
        &[
            &genesis_block_header.block_num().to_raw_sql(),
            genesis_block_header,
            &PartialMmr::default(),
            genesis_commitment,
        ],
    )?;
    Ok(())
}

/// Reads the genesis block commitment from the singleton chain state row, or `None` if the database
/// has not been bootstrapped.
pub fn select_genesis_commitment(tx: &ReadTx<'_>) -> Result<Option<Word>, DatabaseError> {
    Ok(tx
        .query(sql::SELECT_GENESIS_COMMITMENT, &[], |row| row.get::<Word>(0))?
        .into_iter()
        .next())
}

/// Reads the singleton chain state row, returning the persisted block number, header, and chain MMR
/// if any block has been applied locally.
pub fn select_chain_state(
    tx: &ReadTx<'_>,
) -> Result<Option<(BlockNumber, BlockHeader, PartialMmr)>, DatabaseError> {
    Ok(tx
        .query(sql::SELECT_CHAIN_STATE, &[], |row| {
            Ok((
                BlockNumber::from_raw_sql(row.get::<i64>(0)?)?,
                row.get::<BlockHeader>(1)?,
                row.get::<PartialMmr>(2)?,
            ))
        })?
        .into_iter()
        .next())
}
