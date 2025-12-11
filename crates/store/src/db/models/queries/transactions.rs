use std::ops::RangeInclusive;

use diesel::prelude::{Insertable, Queryable};
use diesel::query_dsl::methods::SelectDsl;
use diesel::{
    BoolExpressionMethods,
    ExpressionMethods,
    JoinOnDsl,
    NullableExpressionMethods,
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
use crate::constants::MAX_PAYLOAD_BYTES;
use crate::db::models::conv::SqlTypeConvert;
use crate::db::models::queries::NoteRecordRawRow;
use crate::db::models::queries::notes::NoteRecordWithScriptRawJoined;
use crate::db::models::{serialize_vec, vec_raw_try_into};
use crate::db::schema::note_scripts::{self};
use crate::db::schema::{notes, transactions};
use crate::db::{TransactionRecordWithNotes, TransactionSummary, schema};

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
///     account_id IN (?3)
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
    nullifiers: Vec<u8>,
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
        let nullifiers_binary = self.nullifiers;
        let output_notes_binary = self.output_notes;

        // Deserialize input notes as nullifiers and output notes as note IDs
        let nullifiers: Vec<Nullifier> = Deserializable::read_from_bytes(&nullifiers_binary)?;
        let output_notes: Vec<NoteId> = Deserializable::read_from_bytes(&output_notes_binary)?;

        Ok(crate::db::TransactionRecord {
            account_id: AccountId::read_from_bytes(&self.account_id[..])?,
            block_num: BlockNumber::from_raw_sql(self.block_num)?,
            transaction_id: TransactionId::read_from_bytes(&self.transaction_id[..])?,
            initial_state_commitment: Word::read_from_bytes(&initial_state_commitment)?,
            final_state_commitment: Word::read_from_bytes(&final_state_commitment)?,
            nullifiers,
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
    nullifiers: Vec<u8>,
    output_notes: Vec<u8>,
    size_in_bytes: i64,
}

impl TransactionSummaryRowInsert {
    #[allow(
        clippy::cast_possible_wrap,
        reason = "We will not approach the item count where i64 and usize cause issues"
    )]
    fn new(
        transaction_header: &miden_objects::transaction::TransactionHeader,
        block_num: BlockNumber,
    ) -> Self {
        use miden_lib::utils::Serializable;

        const HEADER_BASE_SIZE: usize = 4 + 32 + 16 + 64; // block_num + tx_id + account_id + commitments

        // Serialize input notes using binary format (store nullifiers)
        let nullifiers_binary = transaction_header.input_notes().to_bytes();

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
        let nullifiers_size = (transaction_header.input_notes().num_notes() * 32) as usize;
        let output_notes_size = transaction_header.output_notes().len() * 500;
        let size_in_bytes = (HEADER_BASE_SIZE + nullifiers_size + output_notes_size) as i64;

        Self {
            transaction_id: transaction_header.id().to_bytes(),
            account_id: transaction_header.account_id().to_bytes(),
            block_num: block_num.to_raw_sql(),
            initial_state_commitment: transaction_header.initial_state_commitment().to_bytes(),
            final_state_commitment: transaction_header.final_state_commitment().to_bytes(),
            nullifiers: nullifiers_binary,
            output_notes: output_notes_binary,
            size_in_bytes,
        }
    }
}

/// Row shape: (`tx`, optional `note`, optional `note_script`)
type TxNoteRow = (TransactionRecordRaw, Option<NoteRecordRawRow>, Option<Vec<u8>>);

/// Select complete transaction records together with their emitted notes using a single join.
///
/// This keeps the same payload limit semantics used by [`select_transactions_records`] and
/// avoids a second round trip to fetch notes separately.
///
/// # Parameters
/// * `account_ids`: List of account IDs to filter by
///     - Limit: 0 <= size <= 1000
/// * `block_range`: Range of blocks to include inclusive
///
/// # Returns
///
/// A tuple containing the last block number included and a vector of
/// [`TransactionRecordWithNotes`] types.
///
/// # Raw SQL
/// ```sql
/// SELECT
///     tx.account_id,
///     tx.block_num,
///     tx.transaction_id,
///     tx.initial_state_commitment,
///     tx.final_state_commitment,
///     tx.nullifiers,
///     tx.output_notes,
///     tx.size_in_bytes,
///
///     n.committed_at,
///     n.batch_index,
///     n.note_index,
///     n.note_id,
///     n.note_commitment,
///     n.note_type,
///     n.sender,
///     n.tag,
///     n.aux,
///     n.execution_hint,
///     n.assets,
///     n.inputs,
///     n.serial_num,
///     n.inclusion_path,
///     n.script_root,
///     s.script  -- nullable
///
/// FROM transactions AS tx
/// LEFT JOIN notes AS n
/// ON tx.block_num = n.committed_at
/// AND tx.account_id = n.sender
/// LEFT JOIN note_scripts AS s
/// ON n.script_root = s.script_root
/// WHERE tx.block_num >= :block_start
/// AND tx.block_num <= :block_end
/// AND tx.account_id IN (:account_ids)
/// ORDER BY
///     tx.block_num ASC,
///     tx.transaction_id ASC,
///     n.batch_index ASC,
///     n.note_index ASC;
/// ```
pub fn select_transactions_records_with_notes(
    conn: &mut SqliteConnection,
    account_ids: &[AccountId],
    block_range: RangeInclusive<BlockNumber>,
) -> Result<(BlockNumber, Vec<TransactionRecordWithNotes>), DatabaseError> {
    QueryParamAccountIdLimit::check(account_ids.len())?;

    if block_range.is_empty() {
        return Err(DatabaseError::InvalidBlockRange {
            from: *block_range.start(),
            to: *block_range.end(),
        });
    }

    let desired_account_ids = serialize_vec(account_ids);

    // FROM transactions AS tx
    let base = schema::transactions::table
        // LEFT JOIN notes AS n ON tx.block_num = n.committed_at AND tx.account_id = n.sender
        .left_join(
            notes::table.on(
                notes::committed_at
                    .eq(transactions::block_num)
                    .and(notes::sender.eq(transactions::account_id)),
            ),
        )
        // LEFT JOIN note_scripts AS s ON n.script_root = s.script_root
        .left_join(
            note_scripts::table.on(
                notes::script_root.eq(note_scripts::script_root.nullable())
            ),
        )
        // WHERE tx.block_num BETWEEN :block_start AND :block_end
        .filter(transactions::block_num.between(block_range.start().to_raw_sql(), block_range.end().to_raw_sql()))
        // AND tx.account_id IN (desired_account_ids)
        .filter(transactions::account_id.eq_any(desired_account_ids));

    // Disambiguate select via UFCS to avoid type ambiguity
    let joined_rows = diesel::query_dsl::QueryDsl::select(
            base,
            (
                TransactionRecordRaw::as_select(),
                Option::<NoteRecordRawRow>::as_select(),
                note_scripts::script.nullable(), // single column from left-joined table
            ),
        )
        // ORDER BY tx.block_num, tx.transaction_id, n.batch_index, n.note_index
        .order((
            transactions::block_num.asc(),
            transactions::transaction_id.asc(),
            notes::batch_index.asc(),
            notes::note_index.asc(),
        ))
        .load::<TxNoteRow>(conn)
        .map_err(DatabaseError::from)?;

    let mut total_size = 0i64;
    let mut limit_hit = false;
    let mut grouped: Vec<(TransactionRecordRaw, Vec<NoteRecordWithScriptRawJoined>)> = Vec::new();

    // Accumulate joined rows per transaction while enforcing the same payload cap.
    let mut current_key: Option<(i64, Vec<u8>)> = None;
    let mut current: Option<(TransactionRecordRaw, Vec<NoteRecordWithScriptRawJoined>)> = None;

    let max_payload = i64::try_from(MAX_PAYLOAD_BYTES).expect("MAX_PAYLOAD_BYTES fits within i64");

    for (tx_raw, note_raw, script) in joined_rows {
        let note = note_raw.map(|note| NoteRecordWithScriptRawJoined::from((note, script.clone())));
        let tx_key = (tx_raw.block_num, tx_raw.transaction_id.clone());

        if current_key.as_ref() != Some(&tx_key) {
            finalize_current(
                &mut current,
                &mut grouped,
                &mut total_size,
                &mut limit_hit,
                max_payload,
            );
            if limit_hit {
                break;
            }
            current_key = Some(tx_key);
            current = Some((tx_raw, note.into_iter().collect()));
            continue;
        }

        if let Some((_, notes)) = &mut current {
            if let Some(note) = note {
                notes.push(note);
            }
        }
    }

    finalize_current(&mut current, &mut grouped, &mut total_size, &mut limit_hit, max_payload);

    let mut last_block_included = *block_range.end();

    // If the payload cap was hit, drop the last (possibly partial) block for consistency.
    if limit_hit && !grouped.is_empty() {
        let last_block_num = grouped
            .last()
            .expect("grouped should be non-empty when limit is hit")
            .0
            .block_num;

        grouped.retain(|(tx, _)| tx.block_num != last_block_num);

        last_block_included = BlockNumber::from_raw_sql(last_block_num.saturating_sub(1))?;
    }

    let transactions = grouped
        .into_iter()
        .map(|(tx_raw, notes_raw)| -> Result<TransactionRecordWithNotes, DatabaseError> {
            Ok(TransactionRecordWithNotes {
                transaction: tx_raw.try_into()?,
                note_records: vec_raw_try_into(notes_raw)?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok((last_block_included, transactions))
}

/// Moves the current accumulated transaction (and its notes) into the grouped list
/// if the payload cap allows it; otherwise marks that the limit was hit.
fn finalize_current(
    current: &mut Option<(TransactionRecordRaw, Vec<NoteRecordWithScriptRawJoined>)>,
    grouped: &mut Vec<(TransactionRecordRaw, Vec<NoteRecordWithScriptRawJoined>)>,
    total_size: &mut i64,
    limit_hit: &mut bool,
    max_payload: i64,
) {
    if let Some((tx_raw, notes)) = current.take() {
        let projected = *total_size + tx_raw.size_in_bytes;
        if projected <= max_payload {
            *total_size = projected;
            grouped.push((tx_raw, notes));
        } else {
            *limit_hit = true;
        }
    }
}
