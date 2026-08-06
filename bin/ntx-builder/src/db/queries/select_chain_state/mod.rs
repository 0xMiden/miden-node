//! Reads the singleton chain state row.

use miden_node_db::sqlite::ReadTx;
use miden_node_db::{DatabaseError, SqlTypeConvert};
use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::crypto::merkle::mmr::PartialMmr;

const SQL: &str = include_str!("select_chain_state.sql");

/// Reads the singleton chain state row, returning the persisted block number, header, and chain MMR
/// if any block has been applied locally.
pub fn select_chain_state(
    tx: &ReadTx<'_>,
) -> Result<Option<(BlockNumber, BlockHeader, PartialMmr)>, DatabaseError> {
    Ok(tx
        .query(SQL, &[], |row| {
            Ok((
                BlockNumber::from_raw_sql(row.get::<i64>(0)?)?,
                row.get::<BlockHeader>(1)?,
                row.get::<PartialMmr>(2)?,
            ))
        })?
        .into_iter()
        .next())
}
