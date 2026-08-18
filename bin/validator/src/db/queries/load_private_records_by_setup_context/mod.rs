//! Reads the encrypted private records belonging to one Golden setup context.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::ReadTx;

use crate::StoredPrivateRecord;
use crate::db::queries::private_record_row::private_record_from_row;

const SQL: &str = include_str!("load_private_records_by_setup_context.sql");

/// Loads every encrypted private record whose shares combine under `setup_context_id`, ordered by
/// transaction id.
pub fn load_private_records_by_setup_context(
    tx: &ReadTx<'_>,
    setup_context_id: [u8; 32],
) -> Result<Vec<StoredPrivateRecord>, DatabaseError> {
    tx.query(SQL, &[&setup_context_id.to_vec()], private_record_from_row)
}
