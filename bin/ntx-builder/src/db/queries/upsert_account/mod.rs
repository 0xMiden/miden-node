//! Inserts or updates the committed state of a network account.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::WriteTx;
use miden_protocol::account::{Account, AccountId};
use miden_protocol::transaction::TransactionId;

const SQL: &str = include_str!("upsert_account.sql");

/// Inserts the committed account state, or updates an existing account's state. In both cases
/// `last_tx_id` is set to the transaction that produced this update.
pub fn upsert_account(
    tx: &WriteTx<'_>,
    account_id: AccountId,
    account: &Account,
    last_tx_id: TransactionId,
) -> Result<(), DatabaseError> {
    tx.execute(SQL, &[&account_id, account, &last_tx_id])?;
    Ok(())
}
