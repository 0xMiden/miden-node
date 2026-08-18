//! Reads the encrypted private records sealed under one storage key epoch.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::ReadTx;

use crate::db::queries::private_record_row::private_record_from_row;
use crate::{StorageKeyEpoch, StoredPrivateRecord};

const SQL: &str = include_str!("load_private_records_by_key_epoch.sql");

/// Loads every encrypted private record sealed under `key_epoch`, ordered by transaction id.
pub fn load_private_records_by_key_epoch(
    tx: &ReadTx<'_>,
    key_epoch: StorageKeyEpoch,
) -> Result<Vec<StoredPrivateRecord>, DatabaseError> {
    tx.query(
        SQL,
        &[&key_epoch.as_bytes().to_vec()],
        private_record_from_row,
    )
}
