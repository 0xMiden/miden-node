use diesel::{RunQueryDsl, SqliteConnection};
use miden_node_store::DatabaseError;
use miden_objects::block::BlockNumber;
use miden_objects::transaction::OrderedTransactionHeaders;

use crate::db::{TransactionSummaryRowInsert, schema};

/// Insert transactions to the DB using the given [`SqliteConnection`].
///
/// # Returns
///
/// The number of affected rows.
///
/// # Note
///
/// The [`SqliteConnection`] object is not consumed. It's up to the caller to commit or rollback the
/// transaction.
pub(crate) fn insert_transactions(
    conn: &mut SqliteConnection,
    block_num: BlockNumber,
    transactions: &OrderedTransactionHeaders,
) -> Result<usize, DatabaseError> {
    #[allow(clippy::into_iter_on_ref)] // false positive
    let rows: Vec<_> = transactions
        .as_slice()
        .into_iter()
        .map(|tx| TransactionSummaryRowInsert::new(tx, block_num))
        .collect();

    let count = diesel::insert_into(schema::transactions::table).values(rows).execute(conn)?;
    Ok(count)
}
