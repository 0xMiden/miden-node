//! Returns an account's vault updates within a block range.

use std::mem::size_of;
use std::ops::RangeInclusive;

use miden_node_db::SqlTypeConvert;
use miden_node_db::sqlite::ReadTx;
use miden_node_utils::limiter::MAX_RESPONSE_PAYLOAD_BYTES;
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::asset::{Asset, AssetId};
use miden_protocol::block::BlockNumber;

use crate::db::AccountVaultValue;
use crate::errors::DatabaseError;

const SQL: &str = include_str!("select_account_vault_assets.sql");

/// A vault update row, with the block number left raw for the trimming below.
type VaultAssetRow = (i64, Word, Option<Asset>);

/// Select account vault assets within a block range (inclusive).
///
/// # Parameters
/// * `account_id`: Account ID to query
/// * `block_range`: Range of block numbers (inclusive)
/// * Response payload size: 0 <= size <= 2MB
///
/// # Returns
///
/// The updates, and the last block the response covers. When the rows would exceed the payload
/// limit, the trailing block is dropped whole.
pub(crate) fn select_account_vault_assets(
    tx: &ReadTx<'_>,
    account_id: AccountId,
    block_range: RangeInclusive<BlockNumber>,
) -> Result<(BlockNumber, Vec<AccountVaultValue>), DatabaseError> {
    // TODO: These limits should be given by the protocol. See miden-protocol/issues/1770 for more
    // details
    const ROW_OVERHEAD_BYTES: usize = 2 * size_of::<Word>() + size_of::<u32>(); // key + asset + block_num
    const MAX_ROWS: usize = MAX_RESPONSE_PAYLOAD_BYTES / ROW_OVERHEAD_BYTES;

    if !account_id.is_public() {
        return Err(DatabaseError::AccountNotPublic(account_id));
    }

    if block_range.is_empty() {
        return Err(DatabaseError::InvalidBlockRange {
            from: *block_range.start(),
            to: *block_range.end(),
        });
    }

    let limit = i64::try_from(MAX_ROWS + 1).expect("should fit within i64");
    let raw =
        tx.query(SQL, &[&account_id, block_range.start(), block_range.end(), &limit], |row| {
            Ok((row.get::<i64>(0)?, row.get::<Word>(1)?, row.get::<Option<Asset>>(2)?))
        })?;

    // If we got more rows than the limit, the last block may be incomplete so we drop it entirely
    // and derive last_block_included from the remaining rows.
    let last_block_num = raw.last().map(|(block_num, ..)| *block_num);
    let (last_block_included, values) = if let Some(last_block_num) = last_block_num
        && raw.len() > MAX_ROWS
    {
        let values = collect_vault_values(
            raw.into_iter().take_while(|(block_num, ..)| *block_num != last_block_num),
        )?;
        let last_block_included = values.last().map_or(*block_range.start(), |v| v.block_num);

        (last_block_included, values)
    } else {
        (*block_range.end(), collect_vault_values(raw)?)
    };

    Ok((last_block_included, values))
}

/// Converts raw `(block_num, vault_key, asset)` rows into [`AccountVaultValue`]s.
fn collect_vault_values(
    rows: impl IntoIterator<Item = VaultAssetRow>,
) -> Result<Vec<AccountVaultValue>, DatabaseError> {
    rows.into_iter()
        .map(|(block_num, vault_key, asset)| {
            Ok(AccountVaultValue {
                block_num: BlockNumber::from_raw_sql(block_num)?,
                vault_key: AssetId::try_from(vault_key)?,
                asset,
            })
        })
        .collect()
}
