use std::ops::RangeInclusive;

use diesel::prelude::{Queryable, QueryableByName};
use diesel::query_dsl::methods::SelectDsl;
use diesel::sqlite::Sqlite;
use diesel::{
    AsChangeset,
    BoolExpressionMethods,
    ExpressionMethods,
    Insertable,
    OptionalExtension,
    QueryDsl,
    RunQueryDsl,
    Selectable,
    SelectableHelper,
    SqliteConnection,
};
use miden_lib::utils::{Deserializable, Serializable};
use miden_node_proto as proto;
use miden_node_proto::domain::account::{AccountInfo, AccountSummary};
use miden_node_utils::limiter::{QueryParamAccountIdLimit, QueryParamLimiter};
use miden_objects::account::delta::AccountUpdateDetails;
use miden_objects::account::{
    Account,
    AccountCode,
    AccountDelta,
    AccountHeader,
    AccountId,
    AccountStorage,
    NonFungibleDeltaAction,
    StorageMap,
    StorageSlot,
    StorageSlotType,
};
use miden_objects::asset::{Asset, AssetVault, AssetVaultKey, FungibleAsset};
use miden_objects::block::{BlockAccountUpdate, BlockNumber};
use miden_objects::{Felt, FieldElement, Word};

use crate::constants::MAX_PAYLOAD_BYTES;
use crate::db::models::conv::{
    SqlTypeConvert,
    nonce_to_raw_sql,
    raw_sql_to_nonce,
    raw_sql_to_slot,
    slot_to_raw_sql,
};
use crate::db::models::{serialize_vec, vec_raw_try_into};
use crate::db::{AccountVaultValue, schema};
use crate::errors::DatabaseError;

/// Select the latest account info by account id from the DB using the given
/// [`SqliteConnection`].
///
/// # Returns
///
/// The latest account info, or an error.
///
/// # Note
///
/// Returns only the account summary. Full account details must be reconstructed
/// in follow up query, using separate query functions to fetch specific account
/// components as needed.
///
/// # Raw SQL
///
/// ```sql
/// SELECT
///     accounts.account_id,
///     accounts.account_commitment,
///     accounts.block_num
/// FROM
///     accounts
/// WHERE
///     account_id = ?1
///     AND is_latest = 1
/// ```
pub(crate) fn select_account(
    conn: &mut SqliteConnection,
    account_id: AccountId,
) -> Result<AccountInfo, DatabaseError> {
    let raw = SelectDsl::select(schema::accounts::table, AccountSummaryRaw::as_select())
        .filter(schema::accounts::account_id.eq(account_id.to_bytes()))
        .filter(schema::accounts::is_latest.eq(true))
        .get_result::<AccountSummaryRaw>(conn)
        .optional()?
        .ok_or(DatabaseError::AccountNotFoundInDb(account_id))?;

    let summary: AccountSummary = raw.try_into()?;

    // Backfill account details from database
    // For private accounts, we don't store full details in the database
    let details = if account_id.is_public() {
        Some(reconstruct_full_account_from_db(conn, account_id)?)
    } else {
        None
    };

    Ok(AccountInfo { summary, details })
}

/// Select the latest account info by account ID prefix from the DB using the given
/// [`SqliteConnection`]. Meant to be used by the network transaction builder.
/// Because network notes get matched through accounts through the account's 30-bit prefix, it is
/// possible that multiple accounts match against a single prefix. In this scenario, the first
/// account is returned.
///
/// # Returns
///
/// The latest account info, `None` if the account was not found, or an error.
///
/// # Raw SQL
///
/// ```sql
/// SELECT
///     accounts.account_id,
///     accounts.account_commitment,
///     accounts.block_num
/// FROM
///     accounts
/// WHERE
///     network_account_id_prefix = ?1
///     AND is_latest = 1
/// ```
pub(crate) fn select_account_by_id_prefix(
    conn: &mut SqliteConnection,
    id_prefix: u32,
) -> Result<Option<AccountInfo>, DatabaseError> {
    let maybe_summary = SelectDsl::select(schema::accounts::table, AccountSummaryRaw::as_select())
        .filter(schema::accounts::is_latest.eq(true))
        .filter(schema::accounts::network_account_id_prefix.eq(Some(i64::from(id_prefix))))
        .get_result::<AccountSummaryRaw>(conn)
        .optional()
        .map_err(DatabaseError::Diesel)?;

    match maybe_summary {
        None => Ok(None),
        Some(raw) => {
            let summary: AccountSummary = raw.try_into()?;
            let account_id = summary.account_id;
            // Backfill account details from database
            let details = reconstruct_full_account_from_db(conn, account_id).ok();
            Ok(Some(AccountInfo { summary, details }))
        },
    }
}

/// Select all account commitments from the DB using the given [`SqliteConnection`].
///
/// # Returns
///
/// The vector with the account id and corresponding commitment, or an error.
///
/// # Raw SQL
///
/// ```sql
/// SELECT
///     account_id,
///     account_commitment
/// FROM
///     accounts
/// WHERE
///     is_latest = 1
/// ORDER BY
///     block_num ASC
/// ```
pub(crate) fn select_all_account_commitments(
    conn: &mut SqliteConnection,
) -> Result<Vec<(AccountId, Word)>, DatabaseError> {
    let raw = SelectDsl::select(
        schema::accounts::table,
        (schema::accounts::account_id, schema::accounts::account_commitment),
    )
    .filter(schema::accounts::is_latest.eq(true))
    .order_by(schema::accounts::block_num.asc())
    .load::<(Vec<u8>, Vec<u8>)>(conn)?;

    Result::<Vec<_>, DatabaseError>::from_iter(raw.into_iter().map(
        |(ref account, ref commitment)| {
            Ok((AccountId::read_from_bytes(account)?, Word::read_from_bytes(commitment)?))
        },
    ))
}

/// Select account vault assets within a block range (inclusive).
///
/// # Parameters
/// * `account_id`: Account ID to query
/// * `block_from`: Starting block number
/// * `block_to`: Ending block number
/// * Response payload size: 0 <= size <= 2MB
/// * Vault assets per response: 0 <= count <= (2MB / (2*Word + u32)) + 1
///
/// # Raw SQL
///
/// ```sql
/// SELECT
///     block_num,
///     vault_key,
///     asset
/// FROM
///     account_vault_assets
/// WHERE
///     account_id = ?1
///     AND block_num >= ?2
///     AND block_num <= ?3
/// ORDER BY
///     block_num ASC
/// LIMIT
///     ?4
/// ```
pub(crate) fn select_account_vault_assets(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    block_range: RangeInclusive<BlockNumber>,
) -> Result<(BlockNumber, Vec<AccountVaultValue>), DatabaseError> {
    use schema::account_vault_assets as t;
    // TODO: These limits should be given by the protocol.
    // See miden-base/issues/1770 for more details
    const ROW_OVERHEAD_BYTES: usize = 2 * size_of::<Word>() + size_of::<u32>(); // key + asset + block_num
    const MAX_ROWS: usize = MAX_PAYLOAD_BYTES / ROW_OVERHEAD_BYTES;

    if !account_id.is_public() {
        return Err(DatabaseError::AccountNotPublic(account_id));
    }

    if block_range.is_empty() {
        return Err(DatabaseError::InvalidBlockRange {
            from: *block_range.start(),
            to: *block_range.end(),
        });
    }

    let raw: Vec<(i64, Vec<u8>, Option<Vec<u8>>)> =
        SelectDsl::select(t::table, (t::block_num, t::vault_key, t::asset))
            .filter(
                t::account_id
                    .eq(account_id.to_bytes())
                    .and(t::block_num.ge(block_range.start().to_raw_sql()))
                    .and(t::block_num.le(block_range.end().to_raw_sql())),
            )
            .order(t::block_num.asc())
            .limit(i64::try_from(MAX_ROWS + 1).expect("should fit within i64"))
            .load::<(i64, Vec<u8>, Option<Vec<u8>>)>(conn)?;

    // Discard the last block in the response (assumes more than one block may be present)
    let (last_block_included, values) = if let Some(&(last_block_num, ..)) = raw.last()
        && raw.len() >= MAX_ROWS
    {
        // NOTE: If the query contains at least one more row than the amount of storage map updates
        // allowed in a single block for an account, then the response is guaranteed to have at
        // least two blocks

        let values = raw
            .into_iter()
            .take_while(|(bn, ..)| *bn != last_block_num)
            .map(AccountVaultValue::from_raw_row)
            .collect::<Result<Vec<_>, DatabaseError>>()?;

        (BlockNumber::from_raw_sql(last_block_num.saturating_sub(1))?, values)
    } else {
        (
            *block_range.end(),
            raw.into_iter().map(AccountVaultValue::from_raw_row).collect::<Result<_, _>>()?,
        )
    };

    Ok((last_block_included, values))
}

/// Select [`AccountSummary`] from the DB using the given [`SqliteConnection`], given that the
/// account update was in the given block range (inclusive).
///
/// # Returns
///
/// The vector of [`AccountSummary`] with the matching accounts.
///
/// # Raw SQL
///
/// ```sql
/// SELECT
///     account_id,
///     account_commitment,
///     block_num
/// FROM
///     accounts
/// WHERE
///     block_num > ?1 AND
///     block_num <= ?2 AND
///     account_id IN (?3)
/// ORDER BY
///     block_num ASC
/// ```
pub fn select_accounts_by_block_range(
    conn: &mut SqliteConnection,
    account_ids: &[AccountId],
    block_range: RangeInclusive<BlockNumber>,
) -> Result<Vec<AccountSummary>, DatabaseError> {
    QueryParamAccountIdLimit::check(account_ids.len())?;

    let desired_account_ids = serialize_vec(account_ids);
    let raw: Vec<AccountSummaryRaw> =
        SelectDsl::select(schema::accounts::table, AccountSummaryRaw::as_select())
            .filter(schema::accounts::block_num.gt(block_range.start().to_raw_sql()))
            .filter(schema::accounts::block_num.le(block_range.end().to_raw_sql()))
            .filter(schema::accounts::account_id.eq_any(desired_account_ids))
            .order(schema::accounts::block_num.asc())
            .load::<AccountSummaryRaw>(conn)?;
    // SAFETY `From` implies `TryFrom<Error=Infallible`, which is the case for `AccountSummaryRaw`
    // -> `AccountSummary`
    Ok(vec_raw_try_into(raw).unwrap())
}

/// Select all accounts from the DB using the given [`SqliteConnection`].
///
/// # Returns
///
/// A vector with accounts, or an error.
///
/// # Raw SQL
///
/// ```sql
/// SELECT
///     accounts.account_id,
///     accounts.account_commitment,
///     accounts.block_num
/// FROM
///     accounts
/// WHERE
///     is_latest = 1
/// ORDER BY
///     block_num ASC
/// ```
#[cfg(test)]
pub(crate) fn select_all_accounts(
    conn: &mut SqliteConnection,
) -> Result<Vec<AccountInfo>, DatabaseError> {
    let raw = SelectDsl::select(schema::accounts::table, AccountSummaryRaw::as_select())
        .filter(schema::accounts::is_latest.eq(true))
        .order_by(schema::accounts::block_num.asc())
        .load::<AccountSummaryRaw>(conn)?;

    let summaries: Vec<AccountSummary> = vec_raw_try_into(raw).unwrap();

    // Backfill account details from database
    let account_infos = summaries
        .into_iter()
        .map(|summary| {
            let account_id = summary.account_id;
            let details = reconstruct_full_account_from_db(conn, account_id).ok();
            AccountInfo { summary, details }
        })
        .collect();

    Ok(account_infos)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageMapValue {
    pub block_num: BlockNumber,
    pub slot_index: u8,
    pub key: Word,
    pub value: Word,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageMapValuesPage {
    /// Highest block number included in `rows`. If the page is empty, this will be `block_from`.
    pub last_block_included: BlockNumber,
    /// Storage map values
    pub values: Vec<StorageMapValue>,
}

impl StorageMapValue {
    pub fn from_raw_row(row: (i64, i32, Vec<u8>, Vec<u8>)) -> Result<Self, DatabaseError> {
        let (block_num, slot_index, key, value) = row;
        Ok(Self {
            block_num: BlockNumber::from_raw_sql(block_num)?,
            slot_index: raw_sql_to_slot(slot_index),
            key: Word::read_from_bytes(&key)?,
            value: Word::read_from_bytes(&value)?,
        })
    }
}

/// Select account storage map values from the DB using the given [`SqliteConnection`].
///
/// # Returns
///
/// A vector of tuples containing `(slot, key, value, is_latest)` for the given account.
/// Each row contains one of:
///
/// - the historical value for a slot and key specifically on block `block_to`
/// - the latest updated value for the slot and key combination, alongside the block number in which
///   it was updated
///
/// # Raw SQL
///
/// ```sql
/// SELECT
///     block_num,
///     slot,
///     key,
///     value
/// FROM
///     account_storage_map_values
/// WHERE
///     account_id = ?1
///     AND block_num >= ?2
///     AND block_num <= ?3
/// ORDER BY
///     block_num ASC
/// LIMIT
///     ?4
/// ```
/// Select account storage map values within a block range (inclusive).
///
/// ## Parameters
///
/// * `account_id`: Account ID to query
/// * `block_range`: Range of block numbers (inclusive)
///
/// ## Response
///
/// * Response payload size: 0 <= size <= 2MB
/// * Storage map values per response: 0 <= count <= (2MB / (2*Word + u32 + u8)) + 1
pub(crate) fn select_account_storage_map_values(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    block_range: RangeInclusive<BlockNumber>,
) -> Result<StorageMapValuesPage, DatabaseError> {
    use schema::account_storage_map_values as t;

    // TODO: These limits should be given by the protocol.
    // See miden-base/issues/1770 for more details
    pub const ROW_OVERHEAD_BYTES: usize =
        2 * size_of::<Word>() + size_of::<u32>() + size_of::<u8>(); // key + value + block_num + slot_idx
    pub const MAX_ROWS: usize = MAX_PAYLOAD_BYTES / ROW_OVERHEAD_BYTES;

    if !account_id.is_public() {
        return Err(DatabaseError::AccountNotPublic(account_id));
    }

    if block_range.is_empty() {
        return Err(DatabaseError::InvalidBlockRange {
            from: *block_range.start(),
            to: *block_range.end(),
        });
    }

    let raw: Vec<(i64, i32, Vec<u8>, Vec<u8>)> =
        SelectDsl::select(t::table, (t::block_num, t::slot, t::key, t::value))
            .filter(
                t::account_id
                    .eq(account_id.to_bytes())
                    .and(t::block_num.ge(block_range.start().to_raw_sql()))
                    .and(t::block_num.le(block_range.end().to_raw_sql())),
            )
            .order(t::block_num.asc())
            .limit(i64::try_from(MAX_ROWS + 1).expect("limit fits within i64"))
            .load(conn)?;

    // Discard the last block in the response (assumes more than one block may be present)

    let (last_block_included, values) = if let Some(&(last_block_num, ..)) = raw.last()
        && raw.len() >= MAX_ROWS
    {
        // NOTE: If the query contains at least one more row than the amount of storage map updates
        // allowed in a single block for an account, then the response is guaranteed to have at
        // least two blocks

        let values = raw
            .into_iter()
            .take_while(|(bn, ..)| *bn != last_block_num)
            .map(StorageMapValue::from_raw_row)
            .collect::<Result<Vec<_>, DatabaseError>>()?;

        (BlockNumber::from_raw_sql(last_block_num.saturating_sub(1))?, values)
    } else {
        (
            *block_range.end(),
            raw.into_iter().map(StorageMapValue::from_raw_row).collect::<Result<_, _>>()?,
        )
    };

    Ok(StorageMapValuesPage { last_block_included, values })
}

/// Reconstruct a `StorageMap` from database entries using `SmtForest`
///
/// This function builds an `SmtForest` from all key-value pairs at the specified block,
/// enabling efficient proof generation with structural sharing. The forest allows
/// maintaining multiple SMT versions in memory with shared nodes.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `account_id` - The account ID
/// * `block_num` - The block number
/// * `slot_index` - The storage slot index
///
/// # Returns
///
/// A reconstructed `StorageMap` backed by `SmtForest` with full proof capabilities.
pub(crate) fn reconstruct_storage_map_at_block(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    block_num: BlockNumber,
    slot_index: u8,
) -> Result<miden_objects::account::StorageMap, DatabaseError> {
    use schema::account_storage_map_values as t;

    // Check if the requested block exists (returns error if not)
    block_exists(conn, block_num)?;

    let account_id_bytes = account_id.to_bytes();
    let block_num_sql = block_num.to_raw_sql();
    let slot_sql = slot_to_raw_sql(slot_index);

    // Query all entries for this slot at or before the given block
    let raw: Vec<(Vec<u8>, Vec<u8>)> = SelectDsl::select(t::table, (t::key, t::value))
        .filter(
            t::account_id
                .eq(&account_id_bytes)
                .and(t::slot.eq(slot_sql))
                .and(t::block_num.le(block_num_sql)),
        )
        .load(conn)?;

    // Parse entries
    let entries: Vec<(Word, Word)> = raw
        .into_iter()
        .map(|(k, v)| Ok((Word::read_from_bytes(&k)?, Word::read_from_bytes(&v)?)))
        .collect::<Result<Vec<_>, DatabaseError>>()?;

    let entry_count = entries.len();

    // StorageMap::with_entries internally uses an SMT which can be backed by SmtForest
    // The SMT is built with structural sharing for memory efficiency
    miden_objects::account::StorageMap::with_entries(entries).map_err(|e| {
        DatabaseError::DataCorrupted(format!(
            "Failed to create StorageMap from {entry_count} entries: {e}"
        ))
    })
}

/// Reconstruct `AccountStorage` from database tables for a specific account at a specific block
///
/// This function queries the `account_storage_headers` table to get slot metadata and reconstructs
/// the `AccountStorage` without deserializing a blob. For Map slots, we only store the commitment
/// since the actual map data is in `account_storage_map_values`.
///
/// # Returns
///
/// The reconstructed `AccountStorage`, or an error if reconstruction fails.
pub(crate) fn select_account_storage_at_block(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    block_num: BlockNumber,
) -> Result<AccountStorage, DatabaseError> {
    use schema::account_storage_headers as t;

    // Check if the requested block exists (returns error if not)
    block_exists(conn, block_num)?;

    let account_id_bytes = account_id.to_bytes();
    let block_num_sql = block_num.to_raw_sql();

    // Query storage headers for this account at this block
    let headers: Vec<AccountStorageHeaderRaw> =
        SelectDsl::select(t::table, AccountStorageHeaderRaw::as_select())
            .filter(t::account_id.eq(&account_id_bytes).and(t::block_num.eq(block_num_sql)))
            .order(t::slot_index.asc())
            .load(conn)?;

    if headers.is_empty() {
        // No storage headers means empty storage
        return Ok(AccountStorage::new(Vec::new())?);
    }

    // Build slots from headers
    let mut slots = Vec::with_capacity(headers.len());

    for header in headers {
        let slot_type = StorageSlotType::from_raw_sql(header.slot_type)?;

        let commitment = Word::read_from_bytes(&header.slot_commitment)?;

        let slot = match slot_type {
            StorageSlotType::Map => {
                // For Map slots, we create an empty map
                // The actual map data is queried separately when needed from
                // account_storage_map_values

                // Create an empty storage map
                let storage_map = StorageMap::new();
                StorageSlot::Map(storage_map)
            },
            StorageSlotType::Value => {
                // For Value slots, the commitment IS the value
                StorageSlot::Value(commitment)
            },
        };

        slots.push(slot);
    }

    Ok(AccountStorage::new(slots)?)
}

/// Select account storage headers at a specific block (lightweight query).
///
/// Returns tuples of `(slot_index, slot_type, commitment)` without reconstructing full slots.
#[allow(dead_code)] // Helper for future SmtForest integration
pub(crate) fn select_account_storage_headers_at_block(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    block_num: BlockNumber,
) -> Result<Vec<(u8, StorageSlotType, Word)>, DatabaseError> {
    use schema::account_storage_headers as t;

    let account_id_bytes = account_id.to_bytes();
    let block_num_sql = block_num.to_raw_sql();

    let headers: Vec<AccountStorageHeaderRaw> =
        SelectDsl::select(t::table, AccountStorageHeaderRaw::as_select())
            .filter(t::account_id.eq(&account_id_bytes).and(t::block_num.le(block_num_sql)))
            .order(t::slot_index.asc())
            .load(conn)?;

    headers
        .into_iter()
        .map(|h| {
            let slot_index = raw_sql_to_slot(h.slot_index);
            let slot_type = StorageSlotType::from_raw_sql(h.slot_type)?;
            let commitment = Word::read_from_bytes(&h.slot_commitment)?;
            Ok((slot_index, slot_type, commitment))
        })
        .collect()
}

/// Reconstruct `AccountStorage` from the latest state in the database
///
/// This queries only the latest storage headers (where `is_latest=true`) for faster reconstruction
/// Select the latest storage headers for an account
///
/// This function queries the `account_storage_headers` table for the latest state of an account's
/// storage slots, using the `is_latest=true` flag for efficiency.
///
/// # Returns
///
/// The reconstructed `AccountStorage` from the latest storage headers.
pub(crate) fn select_latest_account_storage(
    conn: &mut SqliteConnection,
    account_id: AccountId,
) -> Result<AccountStorage, DatabaseError> {
    use schema::account_storage_headers as t;

    let account_id_bytes = account_id.to_bytes();

    // Query latest storage headers for this account
    let headers: Vec<AccountStorageHeaderRaw> =
        SelectDsl::select(t::table, AccountStorageHeaderRaw::as_select())
            .filter(t::account_id.eq(&account_id_bytes).and(t::is_latest.eq(true)))
            .order(t::slot_index.asc())
            .load(conn)?;

    if headers.is_empty() {
        // No storage headers means empty storage
        return Ok(AccountStorage::new(Vec::new())?);
    }

    // Build slots from headers
    let mut slots = Vec::with_capacity(headers.len());

    for header in headers {
        let slot_type = StorageSlotType::from_raw_sql(header.slot_type)?;
        let slot_index = raw_sql_to_slot(header.slot_index);
        let block_num = BlockNumber::from_raw_sql(header.block_num)?;
        let commitment = Word::read_from_bytes(&header.slot_commitment)?;

        let slot = match slot_type {
            StorageSlotType::Map => {
                // For Map slots, reconstruct the full SMT from database entries
                // This allows serving proofs for any key in the map
                let storage_map =
                    reconstruct_storage_map_at_block(conn, account_id, block_num, slot_index)?;
                StorageSlot::Map(storage_map)
            },
            StorageSlotType::Value => {
                // For Value slots, the commitment IS the value
                StorageSlot::Value(commitment)
            },
        };

        slots.push(slot);
    }

    Ok(AccountStorage::new(slots)?)
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = crate::db::schema::account_vault_assets)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct AccountVaultUpdateRaw {
    pub vault_key: Vec<u8>,
    pub asset: Option<Vec<u8>>,
    pub block_num: i64,
}

impl TryFrom<AccountVaultUpdateRaw> for AccountVaultValue {
    type Error = DatabaseError;

    fn try_from(raw: AccountVaultUpdateRaw) -> Result<Self, Self::Error> {
        let vault_key = AssetVaultKey::new_unchecked(Word::read_from_bytes(&raw.vault_key)?);
        let asset = raw.asset.map(|bytes| Asset::read_from_bytes(&bytes)).transpose()?;
        let block_num = BlockNumber::from_raw_sql(raw.block_num)?;

        Ok(AccountVaultValue { block_num, vault_key, asset })
    }
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = schema::account_storage_headers)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
#[allow(dead_code)] // Fields used by Diesel, not directly in Rust code
pub struct AccountStorageHeaderRaw {
    pub account_id: Vec<u8>,
    pub block_num: i64,
    pub slot_index: i32,
    pub slot_type: i32,
    pub slot_commitment: Vec<u8>,
    pub is_latest: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Selectable, Queryable, QueryableByName)]
#[diesel(table_name = schema::accounts)]
#[diesel(check_for_backend(Sqlite))]
pub struct AccountSummaryRaw {
    account_id: Vec<u8>,         // AccountId,
    account_commitment: Vec<u8>, //RpoDigest,
    block_num: i64,              //BlockNumber,
}

impl TryInto<AccountSummary> for AccountSummaryRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<AccountSummary, Self::Error> {
        let account_id = AccountId::read_from_bytes(&self.account_id[..])?;
        let account_commitment = Word::read_from_bytes(&self.account_commitment[..])?;
        let block_num = BlockNumber::from_raw_sql(self.block_num)?;

        Ok(AccountSummary {
            account_id,
            account_commitment,
            block_num,
        })
    }
}

/// Insert an account vault asset row into the DB using the given [`SqliteConnection`].
///
/// Sets `is_latest=true` for the new row and updates any existing
/// row with the same `(account_id, vault_key)` tuple to `is_latest=false`.
///
/// # Returns
///
/// The number of affected rows.
pub(crate) fn insert_account_vault_asset(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    block_num: BlockNumber,
    vault_key: AssetVaultKey,
    asset: Option<Asset>,
) -> Result<usize, DatabaseError> {
    let record = AccountAssetRowInsert::new(&account_id, &vault_key, block_num, asset, true);

    diesel::Connection::transaction(conn, |conn| {
        // First, update any existing rows with the same (account_id, vault_key) to set
        // is_latest=false
        let vault_key: Word = vault_key.into();
        let update_count = diesel::update(schema::account_vault_assets::table)
            .filter(
                schema::account_vault_assets::account_id
                    .eq(&account_id.to_bytes())
                    .and(schema::account_vault_assets::vault_key.eq(&vault_key.to_bytes()))
                    .and(schema::account_vault_assets::is_latest.eq(true)),
            )
            .set(schema::account_vault_assets::is_latest.eq(false))
            .execute(conn)?;

        // Insert the new latest row
        let insert_count = diesel::insert_into(schema::account_vault_assets::table)
            .values(record)
            .execute(conn)?;

        Ok(update_count + insert_count)
    })
}

/// Insert an account storage header into the DB using the given [`SqliteConnection`].
///
/// Sets `is_latest=true` for the new row and updates any existing
/// row with the same (account_id, slot_index) tuple to `is_latest=false`.
///
/// # Returns
///
/// The number of affected rows.
#[cfg(test)]
pub(crate) fn insert_account_storage_header(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    block_num: BlockNumber,
    slot_index: u8,
    slot_type: StorageSlotType,
    slot_commitment: Word,
) -> Result<usize, DatabaseError> {
    use schema::account_storage_headers as t;

    let account_id_bytes = account_id.to_bytes();
    let block_num_sql = block_num.to_raw_sql();
    let slot_index_sql = slot_to_raw_sql(slot_index);
    let slot_type_sql = slot_type.to_raw_sql();
    let slot_commitment_bytes = slot_commitment.to_bytes();

    diesel::Connection::transaction(conn, |conn| {
        // Update existing headers for this slot to set is_latest=false
        let update_count = diesel::update(t::table)
            .filter(
                t::account_id
                    .eq(&account_id_bytes)
                    .and(t::slot_index.eq(slot_index_sql))
                    .and(t::is_latest.eq(true)),
            )
            .set(t::is_latest.eq(false))
            .execute(conn)?;

        // Insert the new latest row
        let insert_count = diesel::insert_into(t::table)
            .values((
                t::account_id.eq(&account_id_bytes),
                t::block_num.eq(block_num_sql),
                t::slot_index.eq(slot_index_sql),
                t::slot_type.eq(slot_type_sql),
                t::slot_commitment.eq(&slot_commitment_bytes),
                t::is_latest.eq(true),
            ))
            .execute(conn)?;

        Ok(update_count + insert_count)
    })
}

/// Insert an account storage map value into the DB using the given [`SqliteConnection`].
///
/// Sets `is_latest=true` for the new row and updates any existing
/// row with the same `(account_id, slot_index, key)` tuple to `is_latest=false`.
///
/// # Returns
///
/// The number of affected rows.
pub(crate) fn insert_account_storage_map_value(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    block_num: BlockNumber,
    slot: u8,
    key: Word,
    value: Word,
) -> Result<usize, DatabaseError> {
    let account_id = account_id.to_bytes();
    let key = key.to_bytes();
    let value = value.to_bytes();
    let slot = slot_to_raw_sql(slot);
    let block_num = block_num.to_raw_sql();

    let update_count = diesel::update(schema::account_storage_map_values::table)
        .filter(
            schema::account_storage_map_values::account_id
                .eq(&account_id)
                .and(schema::account_storage_map_values::slot.eq(slot))
                .and(schema::account_storage_map_values::key.eq(&key))
                .and(schema::account_storage_map_values::is_latest.eq(true)),
        )
        .set(schema::account_storage_map_values::is_latest.eq(false))
        .execute(conn)?;

    let record = AccountStorageMapRowInsert {
        account_id,
        key,
        value,
        slot,
        block_num,
        is_latest: true,
    };
    let insert_count = diesel::insert_into(schema::account_storage_map_values::table)
        .values(record)
        .execute(conn)?;

    Ok(update_count + insert_count)
}

/// Reconstruct full Account from database tables for the latest account state
///
/// This function queries the database tables to reconstruct a complete Account object:
/// - Code from `account_codes` table
/// - Nonce from `accounts` table
/// - Storage from `account_storage_headers` and `account_storage_map_values` tables
/// - Vault from `account_vault_assets` table
///
/// # Note
///
/// A stop-gap solution to retain store API and construct `AccountInfo` types.
/// The function should ultimately be removed, and any queries be served from the
/// `State` which contains an `SmtForest` to serve the latest and most recent
/// historical data.
// TODO: remove eventually once refactoring is complete
fn reconstruct_full_account_from_db(
    conn: &mut SqliteConnection,
    account_id: AccountId,
) -> Result<Account, DatabaseError> {
    // Get account metadata (nonce, code_commitment) and code in a single join query
    let (nonce, code_bytes): (Option<i64>, Vec<u8>) = SelectDsl::select(
        schema::accounts::table.inner_join(schema::account_codes::table),
        (schema::accounts::nonce, schema::account_codes::code),
    )
    .filter(schema::accounts::account_id.eq(account_id.to_bytes()))
    .filter(schema::accounts::is_latest.eq(true))
    .get_result(conn)
    .optional()?
    .ok_or(DatabaseError::AccountNotFoundInDb(account_id))?;

    let nonce = raw_sql_to_nonce(nonce.ok_or_else(|| {
        DatabaseError::DataCorrupted(format!("No nonce found for account {account_id}"))
    })?);

    let code = AccountCode::read_from_bytes(&code_bytes)?;

    // Reconstruct storage using existing helper function
    let storage = select_latest_account_storage(conn, account_id)?;

    // Reconstruct vault from account_vault_assets table
    let vault_entries: Vec<(Vec<u8>, Option<Vec<u8>>)> = SelectDsl::select(
        schema::account_vault_assets::table,
        (schema::account_vault_assets::vault_key, schema::account_vault_assets::asset),
    )
    .filter(schema::account_vault_assets::account_id.eq(account_id.to_bytes()))
    .filter(schema::account_vault_assets::is_latest.eq(true))
    .load(conn)?;

    let mut assets = Vec::new();
    for (_key_bytes, maybe_asset_bytes) in vault_entries {
        if let Some(asset_bytes) = maybe_asset_bytes {
            let asset = Asset::read_from_bytes(&asset_bytes)?;
            assets.push(asset);
        }
    }

    let vault = AssetVault::new(&assets)?;

    Ok(Account::new(account_id, vault, storage, code, nonce, None)?)
}

/// Attention: Assumes the account details are NOT null! The schema explicitly allows this though!
#[allow(clippy::too_many_lines)]
pub(crate) fn upsert_accounts(
    conn: &mut SqliteConnection,
    accounts: &[BlockAccountUpdate],
    block_num: BlockNumber,
) -> Result<usize, DatabaseError> {
    use proto::domain::account::NetworkAccountPrefix;

    let mut count = 0;
    for update in accounts {
        let account_id = update.account_id();

        let network_account_id_prefix = if account_id.is_network() {
            Some(NetworkAccountPrefix::try_from(account_id)?)
        } else {
            None
        };

        // NOTE: we collect storage / asset inserts to apply them only after the  account row is
        // written. The storage and vault tables have FKs pointing to `accounts (account_id,
        // block_num)`, so inserting them earlier would violate those constraints when inserting a
        // brand-new account.
        let (full_account, pending_storage_inserts, pending_asset_inserts) = match update.details()
        {
            AccountUpdateDetails::Private => (None, vec![], vec![]),

            AccountUpdateDetails::Delta(delta) if delta.is_full_state() => {
                let account = Account::try_from(delta)?;
                debug_assert_eq!(account_id, account.id());

                if account.commitment() != update.final_state_commitment() {
                    return Err(DatabaseError::AccountCommitmentsMismatch {
                        calculated: account.commitment(),
                        expected: update.final_state_commitment(),
                    });
                }

                // collect storage-map inserts to apply after account upsert
                let mut storage = Vec::new();
                for (slot_idx, slot) in account.storage().slots().iter().enumerate() {
                    if let StorageSlot::Map(storage_map) = slot {
                        // SAFETY: We can safely unwrap the conversion to u8 because
                        // accounts have a limit of 255 storage elements
                        for (key, value) in storage_map.entries() {
                            storage.push((
                                account_id,
                                u8::try_from(slot_idx).unwrap(),
                                *key,
                                *value,
                            ));
                        }
                    }
                }

                (Some(account), storage, Vec::new())
            },

            AccountUpdateDetails::Delta(delta) => {
                // Reconstruct the full account from database tables
                let account = reconstruct_full_account_from_db(conn, account_id)?;

                // --- collect storage map updates ----------------------------

                let mut storage = Vec::new();
                for (&slot, map_delta) in delta.storage().maps() {
                    for (key, value) in map_delta.entries() {
                        storage.push((account_id, slot, (*key).into(), *value));
                    }
                }

                // apply delta to the account; we need to do this before we process asset updates
                // because we currently need to get the current value of fungible assets from the
                // account
                let account_after = apply_delta(account, delta, &update.final_state_commitment())?;

                // --- process asset updates ----------------------------------

                let mut assets = Vec::new();

                for (faucet_id, _) in delta.vault().fungible().iter() {
                    let current_amount = account_after.vault().get_balance(*faucet_id).unwrap();
                    let asset: Asset = FungibleAsset::new(*faucet_id, current_amount)?.into();
                    let update_or_remove = if current_amount == 0 { None } else { Some(asset) };

                    assets.push((account_id, asset.vault_key(), update_or_remove));
                }

                for (asset, delta_action) in delta.vault().non_fungible().iter() {
                    let asset_update = match delta_action {
                        NonFungibleDeltaAction::Add => Some(Asset::NonFungible(*asset)),
                        NonFungibleDeltaAction::Remove => None,
                    };
                    assets.push((account_id, asset.vault_key(), asset_update));
                }

                (Some(account_after), storage, assets)
            },
        };

        if let Some(code) = full_account.as_ref().map(Account::code) {
            let code_value = AccountCodeRowInsert {
                code_commitment: code.commitment().to_bytes(),
                code: code.to_bytes(),
            };
            diesel::insert_into(schema::account_codes::table)
                .values(&code_value)
                .on_conflict(schema::account_codes::code_commitment)
                .do_nothing()
                .execute(conn)?;
        }

        // mark previous rows as non-latest and insert NEW account row
        diesel::update(schema::accounts::table)
            .filter(
                schema::accounts::account_id
                    .eq(&account_id.to_bytes())
                    .and(schema::accounts::is_latest.eq(true)),
            )
            .set(schema::accounts::is_latest.eq(false))
            .execute(conn)?;

        let account_value = AccountRowInsert {
            account_id: account_id.to_bytes(),
            network_account_id_prefix: network_account_id_prefix
                .map(NetworkAccountPrefix::to_raw_sql),
            account_commitment: update.final_state_commitment().to_bytes(),
            block_num: block_num.to_raw_sql(),
            nonce: full_account.as_ref().map(|account| nonce_to_raw_sql(account.nonce())),
            code_commitment: full_account
                .as_ref()
                .map(|account| account.code().commitment().to_bytes()),
            is_latest: true,
        };

        diesel::insert_into(schema::accounts::table)
            .values(&account_value)
            .execute(conn)?;

        // insert pending storage map entries
        for (acc_id, slot, key, value) in pending_storage_inserts {
            insert_account_storage_map_value(conn, acc_id, block_num, slot, key, value)?;
        }

        // insert pending vault-asset entries
        for (acc_id, vault_key, update) in pending_asset_inserts {
            insert_account_vault_asset(conn, acc_id, block_num, vault_key, update)?;
        }

        count += 1;
    }

    Ok(count)
}

/// Deserializes account and applies account delta.
pub(crate) fn apply_delta(
    mut account: Account,
    delta: &AccountDelta,
    final_state_commitment: &Word,
) -> crate::db::Result<Account, DatabaseError> {
    account.apply_delta(delta)?;

    let actual_commitment = account.commitment();
    if &actual_commitment != final_state_commitment {
        return Err(DatabaseError::AccountCommitmentsMismatch {
            calculated: actual_commitment,
            expected: *final_state_commitment,
        });
    }

    Ok(account)
}

#[derive(Insertable, Debug, Clone)]
#[diesel(table_name = schema::account_codes)]
pub(crate) struct AccountCodeRowInsert {
    pub(crate) code_commitment: Vec<u8>,
    pub(crate) code: Vec<u8>,
}

#[derive(Insertable, AsChangeset, Debug, Clone)]
#[diesel(table_name = schema::account_storage_headers)]
pub(crate) struct AccountStorageHeaderInsert {
    pub(crate) account_id: Vec<u8>,
    pub(crate) block_num: i64,
    pub(crate) slot_index: i32,
    pub(crate) slot_type: i32,
    pub(crate) slot_commitment: Vec<u8>,
    pub(crate) is_latest: bool,
}

#[derive(Insertable, AsChangeset, Debug, Clone)]
#[diesel(table_name = schema::accounts)]
pub(crate) struct AccountRowInsert {
    pub(crate) account_id: Vec<u8>,
    pub(crate) network_account_id_prefix: Option<i64>,
    pub(crate) block_num: i64,
    pub(crate) account_commitment: Vec<u8>,
    pub(crate) code_commitment: Option<Vec<u8>>,
    pub(crate) nonce: Option<i64>,
    pub(crate) is_latest: bool,
}

#[derive(Insertable, AsChangeset, Debug, Clone)]
#[diesel(table_name = schema::account_vault_assets)]
pub(crate) struct AccountAssetRowInsert {
    pub(crate) account_id: Vec<u8>,
    pub(crate) block_num: i64,
    pub(crate) vault_key: Vec<u8>,
    pub(crate) asset: Option<Vec<u8>>,
    pub(crate) is_latest: bool,
}

impl AccountAssetRowInsert {
    pub(crate) fn new(
        account_id: &AccountId,
        vault_key: &AssetVaultKey,
        block_num: BlockNumber,
        asset: Option<Asset>,
        is_latest: bool,
    ) -> Self {
        let account_id = account_id.to_bytes();
        let vault_key: Word = (*vault_key).into();
        let vault_key = vault_key.to_bytes();
        let block_num = block_num.to_raw_sql();
        let asset = asset.map(|asset| asset.to_bytes());
        Self {
            account_id,
            block_num,
            vault_key,
            asset,
            is_latest,
        }
    }
}

#[derive(Insertable, AsChangeset, Debug, Clone)]
#[diesel(table_name = schema::account_storage_map_values)]
pub(crate) struct AccountStorageMapRowInsert {
    pub(crate) account_id: Vec<u8>,
    pub(crate) block_num: i64,
    pub(crate) slot: i32,
    pub(crate) key: Vec<u8>,
    pub(crate) value: Vec<u8>,
    pub(crate) is_latest: bool,
}

/// Queries vault assets (key, value) pairs at a specific block
pub(crate) fn select_account_vault_at_block(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    block_num: BlockNumber,
) -> Result<Vec<(Word, Word)>, DatabaseError> {
    use schema::account_vault_assets as t;

    // Check if the requested block exists (returns error if not)
    block_exists(conn, block_num)?;

    let account_id_bytes = account_id.to_bytes();
    let block_num_sql = i64::from(block_num.as_u32());
    let raw: Vec<(Vec<u8>, Option<Vec<u8>>)> = SelectDsl::select(
        t::table
            .filter(t::account_id.eq(&account_id_bytes))
            .filter(t::block_num.le(block_num_sql))
            .order(t::block_num.desc())
            .limit(1),
        (t::vault_key, t::asset),
    )
    .load(conn)?;

    let entries = raw
        .into_iter()
        .filter_map(|(key_bytes, maybe_asset_bytes)| {
            let key = Word::read_from_bytes(&key_bytes).ok()?;
            let asset_bytes = maybe_asset_bytes?;
            let value = Word::read_from_bytes(&asset_bytes).ok()?;
            Some((key, value))
        })
        .collect();

    Ok(entries)
}

/// Computes the storage commitment from a list of slot commitments.
///
/// This replicates the logic from `AccountStorage::commitment()` which hashes all slot
/// commitments together.
///
/// # Arguments
///
/// * `slot_commitments` - Vector of slot commitment words
///
/// # Returns
///
/// The storage commitment as a `Word`
fn compute_storage_commitment(slot_commitments: &[Word]) -> Word {
    use miden_objects::crypto::hash::rpo::Rpo256;

    let elements: Vec<Felt> = slot_commitments.iter().flat_map(|w| w.iter()).copied().collect();

    Rpo256::hash_elements(&elements)
}

/// Helper function to check if a block exists in the block_headers table.
///
/// This should be called by all `_at_block` query functions to ensure that
/// queries are only performed against blocks that have been produced.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `block_num` - The block number to check
///
/// # Returns
///
/// * `Ok(())` - If the block exists
/// * `Err(DatabaseError::BlockNotFound)` - If the block doesn't exist
/// * `Err(DatabaseError)` - If there's a database error
fn block_exists(conn: &mut SqliteConnection, block_num: BlockNumber) -> Result<(), DatabaseError> {
    use schema::block_headers;

    let count: i64 = SelectDsl::select(
        block_headers::table.filter(block_headers::block_num.eq(block_num.to_raw_sql())),
        diesel::dsl::count(block_headers::block_num),
    )
    .first(conn)?;

    if count > 0 {
        Ok(())
    } else {
        Err(DatabaseError::BlockNotFound(block_num))
    }
}

/// Queries the account code for a specific account at a specific block number.
///
/// Returns `None` if:
/// - The account doesn't exist at that block
/// - The account has no code (private account or account without code commitment)
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `account_id` - The account ID to query
/// * `block_num` - The block number at which to query the account code
///
/// # Returns
///
/// * `Ok(Some(Vec<u8>))` - The account code bytes if found
/// * `Ok(None)` - If account doesn't exist or has no code
/// * `Err(DatabaseError)` - If there's a database error
pub(crate) fn select_account_code_at_block(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    block_num: BlockNumber,
) -> Result<Option<Vec<u8>>, DatabaseError> {
    use schema::{account_codes, accounts};

    // Check if the requested block exists (returns error if not)
    block_exists(conn, block_num)?;

    let account_id_bytes = account_id.to_bytes();
    let block_num_sql = i64::from(block_num.as_u32());
    // Query the accounts table to get the code_commitment at the specified block or earlier
    // Then join with account_codes to get the actual code
    let result: Option<Vec<u8>> = SelectDsl::select(
        accounts::table
            .inner_join(account_codes::table)
            .filter(accounts::account_id.eq(&account_id_bytes))
            .filter(accounts::block_num.le(block_num_sql))
            .order(accounts::block_num.desc())
            .limit(1),
        account_codes::code,
    )
    .first(conn)
    .optional()?;

    Ok(result)
}

/// Queries the account header for a specific account at a specific block number.
///
/// This reconstructs the `AccountHeader` by joining multiple tables:
/// - `accounts` table for `account_id`, `nonce`, `code_commitment`
/// - `account_vault_headers` table for `vault_root`
/// - `account_storage_headers` table for storage slot commitments (to compute `storage_commitment`)
///
/// Returns `None` if the account doesn't exist at that block.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `account_id` - The account ID to query
/// * `block_num` - The block number at which to query the account header
///
/// # Returns
///
/// * `Ok(Some(AccountHeader))` - The account header if found
/// * `Ok(None)` - If account doesn't exist at that block
/// * `Err(DatabaseError)` - If there's a database error
pub(crate) fn select_account_header_at_block(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    block_num: BlockNumber,
) -> Result<Option<AccountHeader>, DatabaseError> {
    use schema::{account_storage_headers, account_vault_headers, accounts};

    // Check if the requested block exists (returns error if not)
    block_exists(conn, block_num)?;

    let account_id_bytes = account_id.to_bytes();
    let block_num_sql = block_num.to_raw_sql();
    let account_data: Option<(Option<Vec<u8>>, Option<i64>)> = SelectDsl::select(
        accounts::table
            .filter(accounts::account_id.eq(&account_id_bytes))
            .filter(accounts::block_num.le(block_num_sql))
            .order(accounts::block_num.desc())
            .limit(1),
        (accounts::code_commitment, accounts::nonce),
    )
    .first(conn)
    .optional()?;

    let Some((code_commitment_bytes, nonce_raw)) = account_data else {
        return Ok(None);
    };
    let vault_root_bytes: Option<Vec<u8>> = SelectDsl::select(
        account_vault_headers::table
            .filter(account_vault_headers::account_id.eq(&account_id_bytes))
            .filter(account_vault_headers::block_num.le(block_num_sql))
            .order(account_vault_headers::block_num.desc())
            .limit(1),
        account_vault_headers::vault_root,
    )
    .first(conn)
    .optional()?;

    let storage_slots: Vec<(i32, i32, Vec<u8>)> = SelectDsl::select(
        account_storage_headers::table
            .filter(account_storage_headers::account_id.eq(&account_id_bytes))
            .filter(account_storage_headers::block_num.le(block_num_sql))
            .order(account_storage_headers::block_num.desc())
            .limit(1),
        (
            account_storage_headers::slot_index,
            account_storage_headers::slot_type,
            account_storage_headers::slot_commitment,
        ),
    )
    .load(conn)?;

    let slot_commitments: Vec<Word> = storage_slots
        .into_iter()
        .map(|(_slot_index, _slot_type, commitment_bytes)| Word::read_from_bytes(&commitment_bytes))
        .collect::<Result<Vec<_>, _>>()?;

    let storage_commitment = compute_storage_commitment(&slot_commitments);

    let code_commitment = code_commitment_bytes
        .map(|bytes| Word::read_from_bytes(&bytes))
        .transpose()?
        .unwrap_or(Word::default());

    let nonce = nonce_raw.map_or(Felt::ZERO, raw_sql_to_nonce);

    let vault_root = vault_root_bytes
        .map(|bytes| Word::read_from_bytes(&bytes))
        .transpose()?
        .unwrap_or(Word::default());

    Ok(Some(AccountHeader::new(
        account_id,
        nonce,
        vault_root,
        storage_commitment,
        code_commitment,
    )))
}
