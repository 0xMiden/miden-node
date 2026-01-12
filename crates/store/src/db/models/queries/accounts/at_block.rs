use std::collections::BTreeMap;

use diesel::prelude::Queryable;
use diesel::query_dsl::methods::SelectDsl;
use diesel::{
    BoolExpressionMethods,
    ExpressionMethods,
    OptionalExtension,
    QueryDsl,
    RunQueryDsl,
    SqliteConnection,
};
use miden_protocol::account::{
    AccountHeader,
    AccountId,
    AccountStorage,
    AccountStorageHeader,
    StorageMap,
    StorageSlot,
    StorageSlotName,
    StorageSlotType,
};
use miden_protocol::asset::Asset;
use miden_protocol::block::BlockNumber;
use miden_protocol::utils::{Deserializable, Serializable};
use miden_protocol::{Felt, FieldElement, Word};

use crate::db::models::conv::{SqlTypeConvert, raw_sql_to_nonce};
use crate::db::schema;
use crate::errors::DatabaseError;

// ACCOUNT HEADER
// ================================================================================================

#[derive(Debug, Clone, Queryable)]
struct AccountHeaderDataRaw {
    code_commitment: Option<Vec<u8>>,
    nonce: Option<i64>,
    storage_header: Option<Vec<u8>>,
    vault_root: Option<Vec<u8>>,
}

/// Queries the account header for a specific account at a specific block number.
///
/// This reconstructs the `AccountHeader` by reading from the `accounts` table:
/// - `account_id`, `nonce`, `code_commitment`, `storage_header`, `vault_root`
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
) -> Result<Option<(AccountHeader, AccountStorageHeader)>, DatabaseError> {
    use schema::accounts;

    let account_id_bytes = account_id.to_bytes();
    let block_num_sql = block_num.to_raw_sql();

    let account_data: Option<AccountHeaderDataRaw> = SelectDsl::select(
        accounts::table
            .filter(accounts::account_id.eq(&account_id_bytes))
            .filter(accounts::block_num.le(block_num_sql))
            .order(accounts::block_num.desc())
            .limit(1),
        (
            accounts::code_commitment,
            accounts::nonce,
            accounts::storage_header,
            accounts::vault_root,
        ),
    )
    .first(conn)
    .optional()?;

    let Some(AccountHeaderDataRaw {
        code_commitment: code_commitment_bytes,
        nonce: nonce_raw,
        storage_header: storage_header_blob,
        vault_root: vault_root_bytes,
    }) = account_data
    else {
        return Ok(None);
    };

    let (storage_commitment, storage_header) = match storage_header_blob {
        Some(blob) => {
            let header = AccountStorageHeader::read_from_bytes(&blob)?;
            let commitment = header.to_commitment();
            (commitment, header)
        },
        None => (Word::default(), AccountStorageHeader::new(Vec::new())?),
    };

    let code_commitment = code_commitment_bytes
        .map(|bytes| Word::read_from_bytes(&bytes))
        .transpose()?
        .unwrap_or(Word::default());

    let nonce = nonce_raw.map_or(Felt::ZERO, raw_sql_to_nonce);

    let vault_root = vault_root_bytes
        .map(|bytes| Word::read_from_bytes(&bytes))
        .transpose()?
        .unwrap_or(Word::default());

    Ok(Some((
        AccountHeader::new(account_id, nonce, vault_root, storage_commitment, code_commitment),
        storage_header,
    )))
}

// ACCOUNT VAULT
// ================================================================================================

/// Query vault assets at a specific block by finding the most recent update for each `vault_key`.
pub(crate) fn select_account_vault_at_block(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    block_num: BlockNumber,
) -> Result<Vec<Asset>, DatabaseError> {
    use schema::account_vault_assets as t;

    let account_id_bytes = account_id.to_bytes();
    let block_num_sql = block_num.to_raw_sql();

    // Since Diesel doesn't support composite keys in subqueries easily, we use a two-step approach:
    // Step 1: Get max block_num for each vault_key
    let latest_blocks_per_vault_key = Vec::from_iter(
        QueryDsl::select(
            t::table
                .filter(t::account_id.eq(&account_id_bytes))
                .filter(t::block_num.le(block_num_sql))
                .group_by(t::vault_key),
            (t::vault_key, diesel::dsl::max(t::block_num)),
        )
        .load::<(Vec<u8>, Option<i64>)>(conn)?
        .into_iter()
        .filter_map(|(key, maybe_block)| maybe_block.map(|block| (key, block))),
    );

    if latest_blocks_per_vault_key.is_empty() {
        return Ok(Vec::new());
    }

    // Step 2: Fetch the full rows matching (vault_key, block_num) pairs
    let mut assets = Vec::new();
    for (vault_key_bytes, max_block) in latest_blocks_per_vault_key {
        // TODO we should not make a query per vault key, but query many at once or
        // or find an alternative approach
        let result: Option<Option<Vec<u8>>> = QueryDsl::select(
            t::table.filter(
                t::account_id
                    .eq(&account_id_bytes)
                    .and(t::vault_key.eq(&vault_key_bytes))
                    .and(t::block_num.eq(max_block)),
            ),
            t::asset,
        )
        .first(conn)
        .optional()?;
        if let Some(Some(asset_bytes)) = result {
            let asset = Asset::read_from_bytes(&asset_bytes)?;
            assets.push(asset);
        }
    }

    // Sort by vault_key for consistent ordering
    assets.sort_by_key(Asset::vault_key);

    Ok(assets)
}

// ACCOUNT STORAGE
// ================================================================================================

/// Returns account storage at a given block by reading from `accounts.storage_header`
/// (which contains the `AccountStorageHeader`) and reconstructing full storage from
/// map values in `account_storage_map_values` table.
pub(crate) fn select_account_storage_at_block(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    block_num: BlockNumber,
) -> Result<AccountStorage, DatabaseError> {
    use schema::account_storage_map_values as t;

    let account_id_bytes = account_id.to_bytes();
    let block_num_sql = block_num.to_raw_sql();

    // Query storage header blob for this account at or before this block
    let storage_blob: Option<Vec<u8>> =
        SelectDsl::select(schema::accounts::table, schema::accounts::storage_header)
            .filter(schema::accounts::account_id.eq(&account_id_bytes))
            .filter(schema::accounts::block_num.le(block_num_sql))
            .order(schema::accounts::block_num.desc())
            .limit(1)
            .first(conn)
            .optional()?
            .flatten();

    let Some(blob) = storage_blob else {
        // No storage means empty storage
        return Ok(AccountStorage::new(Vec::new())?);
    };

    // Deserialize the AccountStorageHeader from the blob
    let header = AccountStorageHeader::read_from_bytes(&blob)?;

    // Query all map values for this account up to and including this block.
    // For each (slot_name, key), we need the latest value at or before block_num.
    // First, get all entries up to block_num
    let map_values: Vec<(i64, String, Vec<u8>, Vec<u8>)> =
        SelectDsl::select(t::table, (t::block_num, t::slot_name, t::key, t::value))
            .filter(t::account_id.eq(&account_id_bytes).and(t::block_num.le(block_num_sql)))
            .order((t::slot_name.asc(), t::key.asc(), t::block_num.desc()))
            .load(conn)?;

    // For each (slot_name, key) pair, keep only the latest entry (highest block_num)
    let mut latest_map_entries: BTreeMap<(StorageSlotName, Word), Word> = BTreeMap::new();

    for (_, slot_name_str, key_bytes, value_bytes) in map_values {
        let slot_name: StorageSlotName = slot_name_str.parse().map_err(|_| {
            DatabaseError::DataCorrupted(format!("Invalid slot name: {slot_name_str}"))
        })?;
        let key = Word::read_from_bytes(&key_bytes)?;
        let value = Word::read_from_bytes(&value_bytes)?;

        // Only insert if we haven't seen this (slot_name, key) yet
        // (since results are ordered by block_num desc, first one is latest)
        latest_map_entries.entry((slot_name, key)).or_insert(value);
    }

    // Group entries by slot name
    let mut map_entries_by_slot: BTreeMap<StorageSlotName, Vec<(Word, Word)>> = BTreeMap::new();
    for ((slot_name, key), value) in latest_map_entries {
        map_entries_by_slot.entry(slot_name).or_default().push((key, value));
    }

    // Reconstruct StorageSlots from header slots + map entries
    let mut slots = Vec::new();
    for slot_header in header.slots() {
        let slot = match slot_header.slot_type() {
            StorageSlotType::Value => {
                // For value slots, the header value IS the slot value
                StorageSlot::with_value(slot_header.name().clone(), slot_header.value())
            },
            StorageSlotType::Map => {
                // For map slots, reconstruct from map entries
                let entries = map_entries_by_slot.remove(slot_header.name()).unwrap_or_default();
                let storage_map = StorageMap::with_entries(entries)?;
                StorageSlot::with_map(slot_header.name().clone(), storage_map)
            },
        };
        slots.push(slot);
    }

    Ok(AccountStorage::new(slots)?)
}
