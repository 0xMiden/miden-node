//! Returns every account's latest committed state.

use miden_node_db::sqlite::ReadTx;
use miden_node_proto::domain::account::AccountInfo;

use crate::db::queries::account_row::account_summary_from_row;
use crate::db::queries::{VALID_FOREVER, select_full_account};
use crate::errors::DatabaseError;

const SQL: &str = include_str!("select_all_account_summaries.sql");

/// Select all accounts from the DB.
///
/// Details are backfilled per account on a best-effort basis, as private accounts have none.
#[cfg(test)]
pub(crate) fn select_all_accounts(tx: &ReadTx<'_>) -> Result<Vec<AccountInfo>, DatabaseError> {
    let summaries = tx.query(SQL, &[&VALID_FOREVER], account_summary_from_row)?;

    Ok(summaries
        .into_iter()
        .map(|summary| {
            let details = select_full_account(tx, summary.account_id).ok();
            AccountInfo { summary, details }
        })
        .collect())
}
