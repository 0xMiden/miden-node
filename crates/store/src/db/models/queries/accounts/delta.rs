//! Optimized delta update support for account updates.
//!
//! Provides functions and types for applying partial delta updates to accounts
//! without loading the full account state. Avoids loading:
//! - Full account code bytes
//! - All storage map entries
//! - All vault assets
//!
//! Instead, only the minimal data needed for the update is fetched.

use std::collections::{BTreeMap, HashMap, HashSet};

use diesel::query_dsl::methods::SelectDsl;
use diesel::{ExpressionMethods, OptionalExtension, QueryDsl, RunQueryDsl, SqliteConnection};
#[cfg(test)]
use miden_protocol::EMPTY_WORD;
use miden_protocol::account::{
    Account,
    AccountId,
    AccountStorageHeader,
    AccountStoragePatch,
    StoragePatchOperation,
    StorageSlotHeader,
    StorageSlotName,
    StorageSlotType,
};
#[cfg(test)]
use miden_protocol::account::{StorageMap, StorageMapKey};
use miden_protocol::utils::serde::{Deserializable, Serializable};
use miden_protocol::{Felt, Word};

use crate::db::models::conv::raw_sql_to_nonce;
use crate::db::schema;
use crate::errors::DatabaseError;

#[cfg(test)]
mod tests;

// TYPES
// ================================================================================================

/// Raw row type for account state delta queries.
///
/// Fields: (`nonce`, `code_commitment`, `storage_header`)
#[derive(diesel::prelude::Queryable)]
struct AccountStateDeltaRow {
    nonce: Option<i64>,
    code_commitment: Option<Vec<u8>>,
    storage_header: Option<Vec<u8>>,
}

/// Data needed for applying a delta update to an existing account. Fetches only the minimal data
/// required, avoiding loading full code and storage.
#[derive(Debug, Clone)]
pub(super) struct AccountStateHeadersForDelta {
    pub nonce: Felt,
    pub code_commitment: Word,
    pub storage_header: AccountStorageHeader,
}

/// Minimal account state computed from a partial delta update. Contains only the fields needed for
/// the accounts table row insert.
#[derive(Debug, Clone)]
pub(super) struct PartialAccountState {
    pub nonce: Felt,
    pub code_commitment: Word,
    pub storage_header: AccountStorageHeader,
    pub vault_root: Word,
}

/// Represents the account state to be inserted, either from a full account or from a partial delta
/// update.
#[expect(
    clippy::large_enum_variant,
    reason = "built per account update and consumed immediately"
)]
pub(super) enum AccountStateForInsert {
    /// Private account - no public state stored
    Private,
    /// Full account state (from full-state delta, i.e., new account)
    FullAccount(Account),
    /// Partial account state (from partial delta, i.e., existing account update)
    PartialState(PartialAccountState),
}

// QUERIES
// ================================================================================================

/// Selects the minimal account state needed for applying a delta update.
///
/// Optimized query that only fetches:
/// - `nonce` (to add `nonce_delta`)
/// - `code_commitment` (unchanged in partial deltas)
/// - `storage_header` (to apply storage delta)
///
/// # Raw SQL
///
/// ```sql
/// SELECT nonce, code_commitment, storage_header
/// FROM accounts
/// WHERE account_id = ?1 AND is_latest = 1
/// ```
pub(super) fn select_minimal_account_state_headers(
    conn: &mut SqliteConnection,
    account_id: AccountId,
) -> Result<AccountStateHeadersForDelta, DatabaseError> {
    let row: AccountStateDeltaRow = SelectDsl::select(
        schema::accounts::table,
        (
            schema::accounts::nonce,
            schema::accounts::code_commitment,
            schema::accounts::storage_header,
        ),
    )
    .filter(schema::accounts::account_id.eq(account_id.to_bytes()))
    .filter(schema::accounts::is_latest.eq(true))
    .get_result(conn)
    .optional()?
    .ok_or(DatabaseError::AccountNotFoundInDb(account_id))?;

    let nonce = raw_sql_to_nonce(row.nonce.ok_or_else(|| {
        DatabaseError::DataCorrupted(format!("No nonce found for account {account_id}"))
    })?);

    let code_commitment = row
        .code_commitment
        .map(|bytes| Word::read_from_bytes(&bytes))
        .transpose()?
        .ok_or_else(|| {
            DatabaseError::DataCorrupted(format!(
                "No code_commitment found for account {account_id}"
            ))
        })?;

    let storage_header = match row.storage_header {
        Some(bytes) => AccountStorageHeader::read_from_bytes(&bytes)?,
        None => AccountStorageHeader::new(Vec::new())?,
    };

    Ok(AccountStateHeadersForDelta { nonce, code_commitment, storage_header })
}

// HELPER FUNCTIONS
// ================================================================================================

/// Applies a storage patch to an existing storage header using precomputed map roots.
///
/// For value slots, updates the slot value directly.
/// For map slots, uses the precomputed roots for updated maps.
/// Removed slots are dropped from the header and created slots are added to it, mirroring
/// [`miden_protocol::account::AccountStorage`]'s patch application.
#[cfg(test)]
pub(super) fn apply_storage_patch(
    header: &AccountStorageHeader,
    patch: &AccountStoragePatch,
    map_entries: &HashMap<StorageSlotName, BTreeMap<StorageMapKey, Word>>,
) -> Result<AccountStorageHeader, DatabaseError> {
    let mut value_updates: HashMap<&StorageSlotName, Word> = HashMap::new();
    let mut map_updates: HashMap<&StorageSlotName, Word> = HashMap::new();
    let mut removed: HashSet<&StorageSlotName> = HashSet::new();

    for (slot_name, value_patch) in patch.values() {
        match value_patch.value() {
            Some(value) => {
                value_updates.insert(slot_name, value);
            },
            None => {
                removed.insert(slot_name);
            },
        }
    }

    for (slot_name, map_patch) in patch.maps() {
        let Some(map_patch_entries) = map_patch.entries() else {
            removed.insert(slot_name);
            continue;
        };
        // Empty entries are a no-op for updates, but creating a map slot with no entries is
        // meaningful and must still produce the slot.
        if map_patch_entries.is_empty() && map_patch.patch_op() != StoragePatchOperation::Create {
            continue;
        }

        let mut entries = map_entries.get(slot_name).cloned().unwrap_or_default();
        for (key, value) in map_patch_entries.as_map() {
            if *value == EMPTY_WORD {
                entries.remove(key);
            } else {
                entries.insert(*key, *value);
            }
        }

        let storage_map =
            StorageMap::with_entries(entries).map_err(DatabaseError::StorageMapError)?;
        map_updates.insert(slot_name, storage_map.root());
    }

    let mut slots =
        Vec::from_iter(header.slots().filter(|slot| !removed.contains(slot.name())).map(|slot| {
            let slot_name = slot.name();
            if let Some(new_value) = value_updates.remove(slot_name) {
                StorageSlotHeader::new(slot_name.clone(), slot.slot_type(), new_value)
            } else if let Some(new_root) = map_updates.remove(slot_name) {
                StorageSlotHeader::new(slot_name.clone(), slot.slot_type(), new_root)
            } else {
                slot.clone()
            }
        }));

    // Any updates left over belong to slots created by the patch.
    for (slot_name, value) in value_updates {
        slots.push(StorageSlotHeader::new(slot_name.clone(), StorageSlotType::Value, value));
    }
    for (slot_name, root) in map_updates {
        slots.push(StorageSlotHeader::new(slot_name.clone(), StorageSlotType::Map, root));
    }

    slots.sort_by_key(StorageSlotHeader::id);

    AccountStorageHeader::new(slots).map_err(|e| {
        DatabaseError::DataCorrupted(format!("Failed to create storage header: {e:?}"))
    })
}

/// Applies a storage patch to an existing storage header using precomputed map roots.
///
/// This mirrors the legacy storage patch path for value-slot updates, map-slot removal, no-op map
/// updates, and slot creation. For map slots whose final root is needed, it uses the root supplied
/// by the caller instead of loading the previous map entries and reconstructing the map.
pub(super) fn apply_storage_patch_with_roots(
    header: &AccountStorageHeader,
    patch: &AccountStoragePatch,
    precomputed_map_roots: &BTreeMap<StorageSlotName, Word>,
) -> Result<AccountStorageHeader, DatabaseError> {
    let mut value_updates: HashMap<&StorageSlotName, Word> = HashMap::new();
    let mut map_updates: HashMap<&StorageSlotName, Word> = HashMap::new();
    let mut removed: HashSet<&StorageSlotName> = HashSet::new();

    for (slot_name, value_patch) in patch.values() {
        match value_patch.value() {
            Some(value) => {
                value_updates.insert(slot_name, value);
            },
            None => {
                removed.insert(slot_name);
            },
        }
    }

    for (slot_name, map_patch) in patch.maps() {
        let Some(map_patch_entries) = map_patch.entries() else {
            removed.insert(slot_name);
            continue;
        };
        // Empty entries are a no-op for updates, but creating a map slot with no entries is
        // meaningful and must still produce the slot.
        if map_patch_entries.is_empty() && map_patch.patch_op() != StoragePatchOperation::Create {
            continue;
        }

        let root = precomputed_map_roots.get(slot_name).copied().ok_or_else(|| {
            DatabaseError::DataCorrupted(format!(
                "missing precomputed storage map root for slot {slot_name}"
            ))
        })?;
        map_updates.insert(slot_name, root);
    }

    let mut slots =
        Vec::from_iter(header.slots().filter(|slot| !removed.contains(slot.name())).map(|slot| {
            let slot_name = slot.name();
            if let Some(new_value) = value_updates.remove(slot_name) {
                StorageSlotHeader::new(slot_name.clone(), slot.slot_type(), new_value)
            } else if let Some(new_root) = map_updates.remove(slot_name) {
                StorageSlotHeader::new(slot_name.clone(), slot.slot_type(), new_root)
            } else {
                slot.clone()
            }
        }));

    // Any updates left over belong to slots created by the patch.
    for (slot_name, value) in value_updates {
        slots.push(StorageSlotHeader::new(slot_name.clone(), StorageSlotType::Value, value));
    }
    for (slot_name, root) in map_updates {
        slots.push(StorageSlotHeader::new(slot_name.clone(), StorageSlotType::Map, root));
    }

    slots.sort_by_key(StorageSlotHeader::id);

    AccountStorageHeader::new(slots).map_err(|e| {
        DatabaseError::DataCorrupted(format!("Failed to create storage header: {e:?}"))
    })
}
