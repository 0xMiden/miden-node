//! Returns account code by its commitment.

use miden_node_db::sqlite::ReadTx;
use miden_protocol::Word;

use crate::errors::DatabaseError;

const SQL: &str = include_str!("select_account_code_by_commitment.sql");

/// Select account code by its commitment hash from the `account_codes` table.
///
/// # Returns
///
/// The account code bytes if found, or `None` if no code exists with that commitment.
pub(crate) fn select_account_code_by_commitment(
    tx: &ReadTx<'_>,
    code_commitment: Word,
) -> Result<Option<Vec<u8>>, DatabaseError> {
    // Invariant: `code_commitment` is the primary key, so there is at most one row.
    Ok(tx
        .query(SQL, &[&code_commitment], |row| row.get::<Vec<u8>>(0))?
        .into_iter()
        .next())
}
