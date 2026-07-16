//! Account-related queries.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::{ReadTx, WriteTx};
use miden_protocol::account::{Account, AccountId};
use miden_protocol::transaction::TransactionId;

use crate::db::sql;

/// Inserts the committed account state, or updates an existing account's state. In both cases
/// `last_tx_id` is set to the transaction that produced this update.
pub fn upsert_account(
    tx: &WriteTx<'_>,
    account_id: AccountId,
    account: &Account,
    last_tx_id: TransactionId,
) -> Result<(), DatabaseError> {
    tx.execute(sql::UPSERT_ACCOUNT, &[&account_id, account, &last_tx_id])?;
    Ok(())
}

/// Returns the latest transaction recorded against `account_id`, or `None` if the account is not
/// tracked locally.
pub fn account_last_tx(
    tx: &ReadTx<'_>,
    account_id: AccountId,
) -> Result<Option<TransactionId>, DatabaseError> {
    Ok(tx
        .query(sql::ACCOUNT_LAST_TX, &[&account_id], |row| row.get::<TransactionId>(0))?
        .into_iter()
        .next())
}

/// Returns `true` if a committed state for the given account is tracked locally.
pub fn account_exists(tx: &ReadTx<'_>, account_id: AccountId) -> Result<bool, DatabaseError> {
    Ok(tx
        .query(sql::ACCOUNT_EXISTS, &[&account_id], |row| row.get::<bool>(0))?
        .into_iter()
        .next()
        .unwrap_or(false))
}

/// Returns the committed account state for the given network account.
pub fn get_account(
    tx: &ReadTx<'_>,
    account_id: AccountId,
) -> Result<Option<Account>, DatabaseError> {
    Ok(tx
        .query(sql::GET_ACCOUNT, &[&account_id], |row| row.get::<Account>(0))?
        .into_iter()
        .next())
}
