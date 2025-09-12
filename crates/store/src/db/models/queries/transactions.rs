#![allow(
    clippy::cast_possible_wrap,
    reason = "We will not approach the item count where i64 and usize cause issues"
)]

use std::ops::RangeInclusive;

use diesel::prelude::{Insertable, Queryable};
use diesel::query_dsl::methods::SelectDsl;
use diesel::{
    ExpressionMethods,
    QueryDsl,
    QueryableByName,
    RunQueryDsl,
    Selectable,
    SelectableHelper,
    SqliteConnection,
};
use miden_lib::utils::Deserializable;
use miden_node_utils::limiter::{QueryParamAccountIdLimit, QueryParamLimiter};
use miden_objects::account::AccountId;
use miden_objects::block::BlockNumber;
use miden_objects::note::{NoteId, Nullifier};
use miden_objects::transaction::{OrderedTransactionHeaders, TransactionId};

use super::DatabaseError;
use crate::db::models::conv::SqlTypeConvert;
use crate::db::models::{serialize_vec, vec_raw_try_into};
use crate::db::{TransactionSummary, schema};

/// Select transactions for given accounts in a specified block range
///
/// # Parameters
/// * `account_ids`: List of account IDs to filter by
///     - Limit: 0 <= size <= 1000
/// * `block_range`: Range of blocks to include inclusive
///
/// # Returns
///
/// A vector of [`TransactionSummary`] types or an error.
///
/// # Raw SQL
/// ```sql
/// SELECT
///     account_id,
///     block_num,
///     transaction_id
/// FROM
///     transactions
/// WHERE
///     block_num > ?1 AND
///     block_num <= ?2 AND
///     account_id IN rarray(?3)
/// ORDER BY
///     transaction_id ASC
/// ```
pub fn select_transactions_by_accounts_and_block_range(
    conn: &mut SqliteConnection,
    account_ids: &[AccountId],
    block_range: RangeInclusive<BlockNumber>,
) -> Result<Vec<TransactionSummary>, DatabaseError> {
    QueryParamAccountIdLimit::check(account_ids.len())?;

    let desired_account_ids = serialize_vec(account_ids);
    let raw = SelectDsl::select(
        schema::transactions::table,
        (
            schema::transactions::account_id,
            schema::transactions::block_num,
            schema::transactions::transaction_id,
        ),
    )
    .filter(schema::transactions::block_num.gt(block_range.start().to_raw_sql()))
    .filter(schema::transactions::block_num.le(block_range.end().to_raw_sql()))
    .filter(schema::transactions::account_id.eq_any(desired_account_ids))
    .order(schema::transactions::transaction_id.asc())
    .load::<TransactionSummaryRaw>(conn)
    .map_err(DatabaseError::from)?;
    vec_raw_try_into(raw)
}

#[derive(Debug, Clone, PartialEq, Queryable, Selectable, QueryableByName)]
#[diesel(table_name = schema::transactions)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct TransactionSummaryRaw {
    account_id: Vec<u8>,
    block_num: i64,
    transaction_id: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Queryable, Selectable, QueryableByName)]
#[diesel(table_name = schema::transactions)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct TransactionHeaderRaw {
    account_id: Vec<u8>,
    block_num: i64,
    transaction_id: Vec<u8>,
    initial_state_commitment: Vec<u8>,
    final_state_commitment: Vec<u8>,
    input_notes: Vec<u8>,
    output_notes: Vec<u8>,
    size_in_bytes: i64,
}

impl TryInto<crate::db::TransactionSummary> for TransactionSummaryRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<crate::db::TransactionSummary, Self::Error> {
        Ok(crate::db::TransactionSummary {
            account_id: AccountId::read_from_bytes(&self.account_id[..])?,
            block_num: BlockNumber::from_raw_sql(self.block_num)?,
            transaction_id: TransactionId::read_from_bytes(&self.transaction_id[..])?,
        })
    }
}

impl TryInto<crate::db::TransactionHeader> for TransactionHeaderRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<crate::db::TransactionHeader, Self::Error> {
        use miden_lib::utils::Deserializable;
        use miden_objects::Word;

        let initial_state_commitment = self.initial_state_commitment;
        let final_state_commitment = self.final_state_commitment;
        let input_notes_binary = self.input_notes;
        let output_notes_binary = self.output_notes;

        // Deserialize input notes as nullifiers and output notes as note IDs
        let input_notes: Vec<Nullifier> = Deserializable::read_from_bytes(&input_notes_binary)?;
        let output_notes: Vec<NoteId> = Deserializable::read_from_bytes(&output_notes_binary)?;

        Ok(crate::db::TransactionHeader {
            account_id: AccountId::read_from_bytes(&self.account_id[..])?,
            block_num: BlockNumber::from_raw_sql(self.block_num)?,
            transaction_id: TransactionId::read_from_bytes(&self.transaction_id[..])?,
            initial_state_commitment: Word::read_from_bytes(&initial_state_commitment)?,
            final_state_commitment: Word::read_from_bytes(&final_state_commitment)?,
            input_notes,
            output_notes,
        })
    }
}

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

#[derive(Debug, Clone, PartialEq, Insertable)]
#[diesel(table_name = schema::transactions)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct TransactionSummaryRowInsert {
    transaction_id: Vec<u8>,
    account_id: Vec<u8>,
    block_num: i64,
    initial_state_commitment: Vec<u8>,
    final_state_commitment: Vec<u8>,
    input_notes: Vec<u8>,
    output_notes: Vec<u8>,
    size_in_bytes: i64,
}

impl TransactionSummaryRowInsert {
    fn new(
        transaction_header: &miden_objects::transaction::TransactionHeader,
        block_num: BlockNumber,
    ) -> Self {
        use miden_lib::utils::Serializable;

        // Serialize input notes using binary format (store nullifiers)
        let input_notes_binary = transaction_header.input_notes().to_bytes();

        // Serialize output notes using binary format (store note IDs)
        let output_notes_binary = transaction_header.output_notes().to_bytes();

        // Store the size required to store the row + the size that the output notes will take when
        // casted to NoteSyncRecord (~500B per output note).
        let size_in_bytes = (transaction_header.to_bytes().len()
            + (transaction_header.output_notes().len() * 500)) as i64;

        Self {
            transaction_id: transaction_header.id().to_bytes(),
            account_id: transaction_header.account_id().to_bytes(),
            block_num: block_num.to_raw_sql(),
            initial_state_commitment: transaction_header.initial_state_commitment().to_bytes(),
            final_state_commitment: transaction_header.final_state_commitment().to_bytes(),
            input_notes: input_notes_binary,
            output_notes: output_notes_binary,
            size_in_bytes,
        }
    }
}

/// Select complete transaction headers for the given accounts and block range.
///
/// # Parameters
/// * `account_ids`: List of account IDs to filter by
///     - Limit: 0 <= size <= 1000
/// * `block_range`: Range of blocks to include inclusive
///
/// # Returns
/// A tuple of (`last_block_included`, `transaction_headers`) where:
/// - `last_block_included`: The highest block number included in the response
/// - `transaction_headers`: Vector of transaction headers, limited by payload size
///
/// # Note
/// This function returns complete transaction header information including state commitments
/// and note IDs, allowing for direct conversion to proto `TransactionHeader` without loading
/// full block data. The response is size-limited to ~5MB to prevent excessive memory usage.
/// If the limit is reached, transactions from the last block are excluded to maintain consistency.
///
/// # Raw SQL
/// ```sql
/// SELECT
///     account_id,
///     block_num,
///     transaction_id,
///     initial_state_commitment,
///     final_state_commitment,
///     input_notes,
///     output_notes
/// FROM transactions
/// WHERE block_num > ?1 AND block_num <= ?2 AND account_id IN rarray(?3)
/// ORDER BY block_num ASC
/// LIMIT ?4
/// ```
pub fn select_transactions_headers(
    conn: &mut SqliteConnection,
    account_ids: &[AccountId],
    block_range: RangeInclusive<BlockNumber>,
) -> Result<(BlockNumber, Vec<crate::db::TransactionHeader>), DatabaseError> {
    const MAX_PAYLOAD_BYTES: usize = 5 * 1024 * 1024; // 5 MB

    QueryParamAccountIdLimit::check(account_ids.len())?;

    if block_range.is_empty() {
        return Err(DatabaseError::InvalidBlockRange {
            from: *block_range.start(),
            to: *block_range.end(),
        });
    }

    let desired_account_ids = serialize_vec(account_ids);
    let raw = SelectDsl::select(schema::transactions::table, TransactionHeaderRaw::as_select())
        .filter(schema::transactions::block_num.gt(block_range.start().to_raw_sql()))
        .filter(schema::transactions::block_num.le(block_range.end().to_raw_sql()))
        .filter(schema::transactions::account_id.eq_any(desired_account_ids))
        .order((schema::transactions::block_num.asc(),))
        .load::<TransactionHeaderRaw>(conn)
        .map_err(DatabaseError::from)?;

    // Remove complete blocks from the end until we're under the size limit
    let mut filtered_raw = raw;
    while !filtered_raw.is_empty() {
        let total_size: i64 = filtered_raw.iter().map(|tx| tx.size_in_bytes).sum();

        // If we're under the limit, we're done
        if total_size <= MAX_PAYLOAD_BYTES as i64 {
            break;
        }

        // Find the last block number and remove all transactions from that block
        let last_block_num = filtered_raw.last().unwrap().block_num;
        filtered_raw.retain(|tx| tx.block_num != last_block_num);
    }

    // Convert to transaction headers
    let headers: Vec<crate::db::TransactionHeader> = filtered_raw
        .into_iter()
        .map(std::convert::TryInto::try_into)
        .collect::<Result<Vec<_>, _>>()?;

    let last_included_block = headers.last().map_or(*block_range.start(), |tx| tx.block_num);

    Ok((last_included_block, headers))
}
