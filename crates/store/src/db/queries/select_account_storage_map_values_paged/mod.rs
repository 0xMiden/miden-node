//! Returns an account's storage map updates within a block range.

use std::ops::RangeInclusive;

use miden_node_db::SqlTypeConvert;
use miden_node_db::sqlite::ReadTx;
use miden_protocol::Word;
use miden_protocol::account::{AccountId, StorageMapKey, StorageSlotName};
use miden_protocol::block::BlockNumber;

use crate::errors::DatabaseError;

const SQL: &str = include_str!("select_account_storage_map_values_paged.sql");

/// A single storage map value at the block it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageMapValue {
    pub block_num: BlockNumber,
    pub slot_name: StorageSlotName,
    pub key: StorageMapKey,
    pub value: Word,
}

/// Page of storage map values returned by [`select_account_storage_map_values_paged`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageMapValuesPage {
    /// Highest block number included in `values`. If the page is empty, this will be `block_from`.
    pub last_block_included: BlockNumber,
    /// Storage map values
    pub values: Vec<StorageMapValue>,
}

/// A storage map value row, with the block number left raw for the trimming below.
type StorageMapValueRow = (i64, StorageSlotName, StorageMapKey, Word);

/// Select account storage map values within a block range (inclusive).
///
/// ## Response
///
/// * Response payload size: 0 <= size <= 2MB
/// * Storage map values per response: 0 <= count <= (2MB / (2*Word + u32 + u8)) + 1
pub(crate) fn select_account_storage_map_values_paged(
    tx: &ReadTx<'_>,
    account_id: AccountId,
    block_range: RangeInclusive<BlockNumber>,
    limit: usize,
) -> Result<StorageMapValuesPage, DatabaseError> {
    if !account_id.is_public() {
        return Err(DatabaseError::AccountNotPublic(account_id));
    }

    if block_range.is_empty() {
        return Err(DatabaseError::InvalidBlockRange {
            from: *block_range.start(),
            to: *block_range.end(),
        });
    }

    let row_limit = i64::try_from(limit + 1).expect("limit fits within i64");
    let raw =
        tx.query(SQL, &[&account_id, block_range.start(), block_range.end(), &row_limit], |row| {
            Ok((
                row.get::<i64>(0)?,
                row.get::<StorageSlotName>(1)?,
                row.get::<StorageMapKey>(2)?,
                row.get::<Word>(3)?,
            ))
        })?;

    // If we got more rows than the limit, the last block may be incomplete so we drop it entirely
    // and derive last_block_included from the remaining rows.
    let last_block_num = raw.last().map(|(block_num, ..)| *block_num);
    let (last_block_included, values) = if let Some(last_block_num) = last_block_num
        && raw.len() > limit
    {
        let values = collect_storage_map_values(
            raw.into_iter().take_while(|(block_num, ..)| *block_num != last_block_num),
        )?;
        let last_block_included = values.last().map_or(*block_range.start(), |v| v.block_num);

        (last_block_included, values)
    } else {
        (*block_range.end(), collect_storage_map_values(raw)?)
    };

    Ok(StorageMapValuesPage { last_block_included, values })
}

/// Converts raw `(block_num, slot_name, key, value)` rows into [`StorageMapValue`]s.
fn collect_storage_map_values(
    rows: impl IntoIterator<Item = StorageMapValueRow>,
) -> Result<Vec<StorageMapValue>, DatabaseError> {
    rows.into_iter()
        .map(|(block_num, slot_name, key, value)| {
            Ok(StorageMapValue {
                block_num: BlockNumber::from_raw_sql(block_num)?,
                slot_name,
                key,
                value,
            })
        })
        .collect()
}
