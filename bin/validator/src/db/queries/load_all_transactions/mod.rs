//! Reads every validated private transaction in local insertion order.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::ReadTx;

use crate::StoredPrivateRecord;
use crate::db::queries::private_record_row::private_record_from_row;

const SQL: &str = include_str!("load_all_transactions.sql");

/// Loads all validated private transactions in insertion order.
pub fn load_all_transactions(tx: &ReadTx<'_>) -> Result<Vec<StoredPrivateRecord>, DatabaseError> {
    tx.query(SQL, &[], private_record_from_row)
}
