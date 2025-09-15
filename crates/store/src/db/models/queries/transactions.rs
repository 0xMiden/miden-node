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
pub struct TransactionRecordRaw {
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

impl TryInto<crate::db::TransactionRecord> for TransactionRecordRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<crate::db::TransactionRecord, Self::Error> {
        use miden_lib::utils::Deserializable;
        use miden_objects::Word;

        let initial_state_commitment = self.initial_state_commitment;
        let final_state_commitment = self.final_state_commitment;
        let input_notes_binary = self.input_notes;
        let output_notes_binary = self.output_notes;

        // Deserialize input notes as nullifiers and output notes as note IDs
        let input_notes: Vec<Nullifier> = Deserializable::read_from_bytes(&input_notes_binary)?;
        let output_notes: Vec<NoteId> = Deserializable::read_from_bytes(&output_notes_binary)?;

        Ok(crate::db::TransactionRecord {
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

        // Manually calculate the estimated size of the transaction header to avoid
        // the cost of serialization. The size estimation includes:
        // - 4 bytes for block number
        // - 32 bytes for transaction ID
        // - 16 bytes for account ID
        // - 64 bytes for initial + final state commitments (32 bytes each)
        // - 32 bytes per input note (nullifier size)
        // - 500 bytes per output note (estimated size when converted to NoteSyncRecord)
        //
        // Note: 500 bytes per output note is an over-estimate but ensures we don't
        // exceed memory limits when these transactions are later converted to proto records.
        let header_base_size = 4 + 32 + 16 + 64; // block_num + tx_id + account_id + commitments
        let input_notes_size = transaction_header.input_notes().len() * 32;
        let output_notes_size = transaction_header.output_notes().len() * 500;
        let size_in_bytes = (header_base_size + input_notes_size + output_notes_size) as i64;

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

/// Select complete transaction records for the given accounts and block range.
///
/// # Parameters
/// * `account_ids`: List of account IDs to filter by
///     - Limit: 0 <= size <= 1000
/// * `block_range`: Range of blocks to include inclusive
///
/// # Returns
/// A tuple of (`last_block_included`, `transaction_records`) where:
/// - `last_block_included`: The highest block number included in the response
/// - `transaction_records`: Vector of transaction records, limited by payload size
///
/// # Note
/// This function returns complete transaction record information including state commitments
/// and note IDs, allowing for direct conversion to proto `TransactionRecord` without loading
/// full block data. We use a chunked loading strategy to prevent memory exhaustion attacks and
/// ensure predictable resource usage.
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
///     output_notes,
///     size_in_bytes
/// FROM transactions
/// WHERE block_num > ?1 AND block_num <= ?2 AND account_id IN rarray(?3)
/// ORDER BY block_num ASC
/// LIMIT ?4 OFFSET ?5
/// ```
/// Note: The query is executed in chunks of 1000 transactions to prevent loading
/// excessive data and to stop as soon as the accumulated size approaches the 5MB limit.
pub fn select_transactions_records(
    conn: &mut SqliteConnection,
    account_ids: &[AccountId],
    block_range: RangeInclusive<BlockNumber>,
) -> Result<(BlockNumber, Vec<crate::db::TransactionRecord>), DatabaseError> {
    const MAX_PAYLOAD_BYTES: usize = 5 * 1024 * 1024; // 5 MB
    const CHUNK_SIZE: i64 = 1000; // Read 1000 transactions at a time

    QueryParamAccountIdLimit::check(account_ids.len())?;

    if block_range.is_empty() {
        return Err(DatabaseError::InvalidBlockRange {
            from: *block_range.start(),
            to: *block_range.end(),
        });
    }

    let desired_account_ids = serialize_vec(account_ids);

    // Read transactions in chunks to prevent loading excessive data and to stop
    // as soon as we approach the size limit
    let mut all_transactions = Vec::new();
    let mut current_size = 0i64;
    let mut offset = 0i64;

    loop {
        let chunk =
            SelectDsl::select(schema::transactions::table, TransactionRecordRaw::as_select())
                .filter(schema::transactions::block_num.gt(block_range.start().to_raw_sql()))
                .filter(schema::transactions::block_num.le(block_range.end().to_raw_sql()))
                .filter(schema::transactions::account_id.eq_any(&desired_account_ids))
                .order((schema::transactions::block_num.asc(),))
                .offset(offset)
                .limit(CHUNK_SIZE)
                .load::<TransactionRecordRaw>(conn)
                .map_err(DatabaseError::from)?;

        // If no more data, we're done
        if chunk.is_empty() {
            break;
        }

        // Calculate the size of this chunk
        let chunk_size: i64 = chunk.iter().map(|tx| tx.size_in_bytes).sum();

        // If adding this chunk would exceed the limit, we need to be more careful
        if current_size + chunk_size > MAX_PAYLOAD_BYTES as i64 {
            // Add transactions from this chunk one by one until we hit the limit
            for tx in chunk {
                if current_size + tx.size_in_bytes <= MAX_PAYLOAD_BYTES as i64 {
                    current_size += tx.size_in_bytes;
                    all_transactions.push(tx);
                } else {
                    // Can't fit this transaction, stop here
                    break;
                }
            }
            break;
        }

        // The entire chunk fits, add it all
        current_size += chunk_size;
        let chunk_len = chunk.len() as i64;
        all_transactions.extend(chunk);
        offset += CHUNK_SIZE;

        // If we got fewer transactions than requested, we've reached the end
        if chunk_len < CHUNK_SIZE {
            break;
        }
    }

    // Ensure block consistency: remove complete blocks from the end until we're under the size
    // limit
    let mut filtered_raw = all_transactions;
    while !filtered_raw.is_empty() {
        let total_size: i64 = filtered_raw.iter().map(|tx| tx.size_in_bytes).sum();

        // If we're under the limit, we're done
        if total_size <= MAX_PAYLOAD_BYTES as i64 {
            break;
        }

        // Find the last block number and remove all transactions from that block
        if let Some(last_tx) = filtered_raw.last() {
            let last_block_num = last_tx.block_num;
            filtered_raw.retain(|tx| tx.block_num != last_block_num);
        } else {
            // This shouldn't happen since we check !filtered_raw.is_empty() above, but be safe
            break;
        }
    }

    // Convert to transaction records
    let headers: Vec<crate::db::TransactionRecord> = filtered_raw
        .into_iter()
        .map(std::convert::TryInto::try_into)
        .collect::<Result<Vec<_>, _>>()?;

    let last_included_block = headers.last().map_or(*block_range.start(), |tx| tx.block_num);

    Ok((last_included_block, headers))
}
