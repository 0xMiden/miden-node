//! Reads one encrypted private record by transaction id.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::ReadTx;
use miden_protocol::transaction::TransactionId;

use crate::StoredPrivateRecord;
use crate::db::queries::private_record_row::private_record_from_row;

const SQL: &str = include_str!("load_private_record.sql");

/// Loads the encrypted private record of `transaction_id`, or `None` if the transaction has not
/// been validated by this validator.
pub fn load_private_record(
    tx: &ReadTx<'_>,
    transaction_id: TransactionId,
) -> Result<Option<StoredPrivateRecord>, DatabaseError> {
    Ok(tx.query(SQL, &[&transaction_id], private_record_from_row)?.into_iter().next())
}
