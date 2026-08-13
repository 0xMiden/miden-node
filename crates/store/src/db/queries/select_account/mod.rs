//! Returns an account's latest committed summary, with full details for public accounts.

use miden_node_db::sqlite::ReadTx;
use miden_node_proto::domain::account::AccountInfo;
use miden_protocol::account::AccountId;

use crate::db::queries::account_row::account_summary_from_row;
use crate::db::queries::{VALID_FOREVER, select_full_account};
use crate::errors::DatabaseError;

const SQL: &str = include_str!("select_account_summary.sql");

/// Select account by ID.
///
/// # Returns
///
/// The latest account info, or an error.
pub(crate) fn select_account(
    tx: &ReadTx<'_>,
    account_id: AccountId,
) -> Result<AccountInfo, DatabaseError> {
    let summary = tx
        .query(SQL, &[&account_id, &VALID_FOREVER], account_summary_from_row)?
        .into_iter()
        .next()
        .ok_or(DatabaseError::AccountNotFoundInDb(account_id))?;

    // Backfill account details from database. For private accounts, we don't store full details in
    // the database
    let details = if account_id.is_public() {
        Some(select_full_account(tx, account_id)?)
    } else {
        None
    };

    Ok(AccountInfo { summary, details })
}
