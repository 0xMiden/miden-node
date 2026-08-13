//! Database query functions for the store.
//!
//! Each function takes a [`ReadTx`](miden_node_db::sqlite::ReadTx) or
//! [`WriteTx`](miden_node_db::sqlite::WriteTx) and is driven from a [`Db`](crate::db::Db) method
//! through [`DbReader::read`](miden_node_db::sqlite::DbReader::read) /
//! [`DbWriter::write`](miden_node_db::sqlite::DbWriter::write). One module per query, holding the
//! function and the `.sql` file it executes.

mod account_row;
mod block_header_row;
mod note_row;

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::{DbValue, DbValueRef, FromSqlValue, InList, ToSqlValue};

// SHARED COLUMN TYPES
// =================================================================================================

/// Sentinel `valid_until` value marking a row as the current, open-ended version of its key.
///
/// Versioned rows (`accounts`, `account_vault_assets`, `account_storage_map_values`) are
/// applicable for blocks in `[block_num, valid_until)`; updating a key closes the previous row's
/// interval by setting its `valid_until` to the new row's `block_num`. The open end is `i64::MAX`
/// rather than NULL so every validity predicate is a single range comparison that partial indexes
/// can serve.
pub(crate) const VALID_FOREVER: i64 = i64::MAX;

/// Classifies accounts for database storage based on whether they are network accounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub(crate) enum NetworkAccountType {
    /// Not a network account.
    None = 0,
    /// A network account.
    Network = 1,
}

impl ToSqlValue for NetworkAccountType {
    fn to_sql_value(&self) -> DbValue {
        DbValue::integer(*self as i64)
    }
}

impl FromSqlValue for NetworkAccountType {
    fn from_sql_value(value: DbValueRef<'_>) -> Result<Self, DatabaseError> {
        match value.as_i64()? {
            0 => Ok(Self::None),
            1 => Ok(Self::Network),
            other => Err(DatabaseError::deserialization(
                "NetworkAccountType",
                InvalidNetworkAccountType(other),
            )),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid network account type value {0}")]
struct InvalidNetworkAccountType(i64);

/// Binds note tags for an `IN` list.
///
/// Tags occupy the full `u32` range and are stored unsigned (see the `NoteTag` codec), so widening
/// them to `i64` is the same value the column holds.
fn note_tag_in_list(note_tags: &[u32]) -> InList {
    InList::from_i64s(note_tags.iter().copied().map(i64::from))
}

// BLOCK QUERIES
// =================================================================================================

mod insert_block_header;
pub(crate) use insert_block_header::insert_block_header;

mod select_all_block_header_commitments;
pub(crate) use select_all_block_header_commitments::select_all_block_header_commitments;

mod select_block_header_and_signatures_by_block_num;
pub(crate) use select_block_header_and_signatures_by_block_num::select_block_header_and_signatures_by_block_num;

mod select_block_header_by_block_num;
pub(crate) use select_block_header_by_block_num::select_block_header_by_block_num;

mod select_block_headers;
pub(crate) use select_block_headers::select_block_headers;

// NOTE QUERIES
// =================================================================================================

mod insert_note_scripts;
pub(crate) use insert_note_scripts::insert_note_scripts;

mod insert_notes;
pub(crate) use insert_notes::insert_notes;

mod get_note_sync_multi;
pub(crate) use get_note_sync_multi::get_note_sync_multi;
#[cfg(test)]
pub(crate) use get_note_sync_multi::{NOTE_SYNC_BLOCK_OVERHEAD_BYTES, NOTE_SYNC_RECORD_BYTES};

mod select_existing_note_commitments;
pub(crate) use select_existing_note_commitments::select_existing_note_commitments;

mod select_note_ids_by_nullifier;
pub(crate) use select_note_ids_by_nullifier::select_note_ids_by_nullifier;

mod select_note_inclusion_proofs;
pub(crate) use select_note_inclusion_proofs::select_note_inclusion_proofs;

mod select_note_script_by_root;
pub(crate) use select_note_script_by_root::select_note_script_by_root;

mod select_note_sync_records;
pub(crate) use select_note_sync_records::select_note_sync_records;

mod select_notes_by_id;
pub(crate) use select_notes_by_id::select_notes_by_id;

mod select_notes_since_block_by_tag;
pub(crate) use select_notes_since_block_by_tag::select_notes_since_block_by_tag;

// NULLIFIER QUERIES
// =================================================================================================

mod insert_nullifiers_for_block;
pub(crate) use insert_nullifiers_for_block::insert_nullifiers_for_block;

#[cfg(test)]
mod select_all_nullifiers;
#[cfg(test)]
pub(crate) use select_all_nullifiers::select_all_nullifiers;

mod select_nullifiers_by_prefix;
pub(crate) use select_nullifiers_by_prefix::select_nullifiers_by_prefix;

mod select_nullifiers_paged;
// `NullifiersPage` is part of the store's public API, so it is re-exported `pub` through the
// private module chain and made public again by `crate::db`.
pub use select_nullifiers_paged::NullifiersPage;
pub(crate) use select_nullifiers_paged::select_nullifiers_paged;

// TRANSACTION QUERIES
// =================================================================================================

mod insert_transactions;
pub(crate) use insert_transactions::insert_transactions;

mod select_transactions_records;
pub(crate) use select_transactions_records::select_transactions_records;

// ACCOUNT QUERIES
// =================================================================================================

mod insert_account_storage_map_value;
pub(crate) use insert_account_storage_map_value::insert_account_storage_map_value;

mod insert_account_vault_asset;
pub(crate) use insert_account_vault_asset::insert_account_vault_asset;

mod prune_history;
pub use prune_history::HISTORICAL_BLOCK_RETENTION;
pub(crate) use prune_history::prune_history;

mod upsert_accounts;
pub(crate) use upsert_accounts::{AccountRow, upsert_accounts};
pub use upsert_accounts::{PrecomputedPublicAccountState, PrecomputedPublicAccountStates};

mod select_account_code_by_commitment;
pub(crate) use select_account_code_by_commitment::select_account_code_by_commitment;

mod select_account_commitments_paged;
pub use select_account_commitments_paged::AccountCommitmentsPage;
pub(crate) use select_account_commitments_paged::select_account_commitments_paged;

mod select_network_accounts_subset;
pub(crate) use select_network_accounts_subset::select_network_accounts_subset;

mod select_public_account_ids_paged;
pub use select_public_account_ids_paged::PublicAccountIdsPage;
pub(crate) use select_public_account_ids_paged::select_public_account_ids_paged;

mod select_public_account_state_roots_paged;
pub use select_public_account_state_roots_paged::PublicAccountStateRootsPage;
pub(crate) use select_public_account_state_roots_paged::select_public_account_state_roots_paged;

mod select_account;
pub(crate) use select_account::select_account;

mod select_account_header_with_storage_header_at_block;
pub(crate) use select_account_header_with_storage_header_at_block::select_account_header_with_storage_header_at_block;

mod select_account_storage_map_values_paged;
#[cfg(test)]
pub(crate) use select_account_storage_map_values_paged::StorageMapValue;
pub use select_account_storage_map_values_paged::StorageMapValuesPage;
pub(crate) use select_account_storage_map_values_paged::select_account_storage_map_values_paged;

mod select_account_vault_assets;
pub(crate) use select_account_vault_assets::select_account_vault_assets;

mod select_account_vault_at_block;
pub(crate) use select_account_vault_at_block::select_account_vault_at_block;

#[cfg(test)]
mod select_all_accounts;
#[cfg(test)]
pub(crate) use select_all_accounts::select_all_accounts;

mod select_full_account;
pub(crate) use select_full_account::select_full_account;

mod select_latest_account_storage;
pub(crate) use select_latest_account_storage::select_latest_account_storage;

// BLOCK APPLICATION
// =================================================================================================

mod apply_block;
pub(crate) use apply_block::apply_block;
