//! Returns full transaction records for a set of accounts within a block range.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;

use miden_node_db::SqlTypeConvert;
use miden_node_db::sqlite::{InList, ReadTx};
use miden_node_utils::limiter::{
    MAX_RESPONSE_PAYLOAD_BYTES,
    QueryParamAccountIdLimit,
    QueryParamLimiter,
    QueryParamNoteCommitmentLimit,
};
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::block::BlockNumber;
use miden_protocol::note::{NoteHeader, NoteId, Nullifier};
use miden_protocol::transaction::{
    InputNoteCommitment,
    InputNotes,
    TransactionHeader,
    TransactionId,
};
use miden_protocol::utils::serde::{Deserializable, Serializable};

use crate::db::TransactionRecord;
use crate::db::queries::{select_note_ids_by_nullifier, select_note_sync_records};
use crate::errors::DatabaseError;

const SQL_FIRST_CHUNK: &str = include_str!("select_transactions_records_chunk.sql");
const SQL_AFTER_CURSOR: &str = include_str!("select_transactions_records_chunk_after.sql");

/// A transaction row, before its notes are resolved.
struct TransactionRow {
    account_id: AccountId,
    block_num: i64,
    transaction_id: TransactionId,
    initial_state_commitment: Word,
    final_state_commitment: Word,
    input_notes: Vec<u8>,
    output_notes: Vec<u8>,
    size_in_bytes: i64,
}

/// Returns the transactions of `account_ids` within `block_range`, and the last block the response
/// covers.
///
/// Notes:
/// - Uses stable ordering (`block_num`, `transaction_id`) to ensure consistent results across
///   paginated queries.
/// - Uses cursor-based pagination.
/// - The query is executed in chunks of 1000 transactions to prevent loading excessive data and to
///   stop as soon as the accumulated size approaches the 4MB limit.
/// - Given the size of note records, 1000 records are guaranteed never to return more than about
///   60MB of data.
pub(crate) fn select_transactions_records(
    tx: &ReadTx<'_>,
    account_ids: &[AccountId],
    block_range: RangeInclusive<BlockNumber>,
) -> Result<(BlockNumber, Vec<TransactionRecord>), DatabaseError> {
    const NUM_TXS_PER_CHUNK: i64 = 1000; // Read 1000 transactions at a time

    QueryParamAccountIdLimit::check(account_ids.len())?;

    let max_payload_bytes =
        i64::try_from(MAX_RESPONSE_PAYLOAD_BYTES).expect("payload limit fits within i64");

    if block_range.is_empty() {
        return Err(DatabaseError::InvalidBlockRange {
            from: *block_range.start(),
            to: *block_range.end(),
        });
    }

    let account_id_bytes = Vec::from_iter(account_ids.iter().map(Serializable::to_bytes));
    let desired_account_ids = InList::from_blobs(account_id_bytes.iter().map(Vec::as_slice));

    // Read transactions in chunks to prevent loading excessive data and to stop as soon as we
    // approach the size limit
    let mut transactions = Vec::new();
    let mut total_size = 0i64;
    let mut cursor: Option<(i64, Vec<u8>)> = None;
    // Track the block number of the first transaction that did not fit within the payload cap. This
    // is the explicit "we truncated" signal; the accumulated byte total cannot be used as a proxy,
    // since a transaction can fail to fit while `total_size` is still below the cap.
    let mut truncated_at_block: Option<i64> = None;

    loop {
        // Apply cursor-based pagination using the last seen (block_num, transaction_id)
        let chunk = match &cursor {
            Some((last_block, last_tx_id)) => tx.query(
                SQL_AFTER_CURSOR,
                &[
                    block_range.start(),
                    block_range.end(),
                    &desired_account_ids,
                    &NUM_TXS_PER_CHUNK,
                    last_block,
                    last_tx_id,
                ],
                transaction_row_from_row,
            )?,
            None => tx.query(
                SQL_FIRST_CHUNK,
                &[block_range.start(), block_range.end(), &desired_account_ids, &NUM_TXS_PER_CHUNK],
                transaction_row_from_row,
            )?,
        };

        // Add transactions from this chunk one by one until we hit the limit
        let mut added_from_chunk = 0;

        for row in chunk {
            if total_size + row.size_in_bytes <= max_payload_bytes {
                total_size += row.size_in_bytes;
                cursor = Some((row.block_num, row.transaction_id.to_bytes()));
                transactions.push(row);
                added_from_chunk += 1;
            } else {
                // This transaction does not fit, so the response is truncated at its block.
                truncated_at_block = Some(row.block_num);
                break;
            }
        }

        // Break if we truncated due to the payload cap, or the chunk was incomplete (i.e. the
        // matching transactions are exhausted).
        if truncated_at_block.is_some() || added_from_chunk < NUM_TXS_PER_CHUNK {
            break;
        }
    }

    let Some(truncation_block) = truncated_at_block else {
        // Every matching transaction in the range fit within the payload cap.
        return Ok((*block_range.end(), with_output_note_proofs(tx, transactions)?));
    };

    // We stopped within `truncation_block`, so that block may be partial. Block-based pagination
    // can only report fully-included blocks, so drop every transaction belonging to the truncation
    // block and report the previous block as the cursor. Transactions are ordered ascending by
    // block number, so the truncation block's transactions form a contiguous suffix:
    // `partition_point` locates the boundary and `truncate` drops the suffix in place, without
    // allocating a new vector, with O(log n) complexity.
    let complete_len = transactions.partition_point(|row| row.block_num < truncation_block);
    transactions.truncate(complete_len);

    if transactions.is_empty() {
        // A single block's transactions exceed the payload cap. Reporting `truncation_block - 1`
        // here would tell the client to resume from `truncation_block`, which can never fit, so
        // pagination would loop forever. Surface the condition instead of silently looping.
        return Err(DatabaseError::TransactionPageExceedsPayloadLimit {
            block_num: BlockNumber::from_raw_sql(truncation_block)?,
        });
    }

    // SAFETY: block_num came from the database and was previously validated. Subtraction is safe
    // under the assumption that genesis block (where it could fail) does not have any transactions.
    let last_included_block = BlockNumber::from_raw_sql(truncation_block.saturating_sub(1))?;
    Ok((last_included_block, with_output_note_proofs(tx, transactions)?))
}

/// Maps a transaction row, leaving the note blobs to be deserialized in bulk afterwards.
fn transaction_row_from_row(
    row: &miden_node_db::sqlite::Row<'_>,
) -> Result<TransactionRow, miden_node_db::DatabaseError> {
    Ok(TransactionRow {
        account_id: row.get::<AccountId>(0)?,
        block_num: row.get::<i64>(1)?,
        transaction_id: row.get::<TransactionId>(2)?,
        initial_state_commitment: row.get::<Word>(3)?,
        final_state_commitment: row.get::<Word>(4)?,
        input_notes: row.get::<Vec<u8>>(5)?,
        output_notes: row.get::<Vec<u8>>(6)?,
        size_in_bytes: row.get::<i64>(7)?,
    })
}

/// Resolves each transaction's committed output notes and consumed note references.
fn with_output_note_proofs(
    tx: &ReadTx<'_>,
    raw_transactions: Vec<TransactionRow>,
) -> Result<Vec<TransactionRecord>, DatabaseError> {
    // Pre-deserialize output notes to collect IDs for the batch lookup.
    let mut tx_output_notes = Vec::with_capacity(raw_transactions.len());
    let mut all_note_ids: Vec<NoteId> = Vec::new();
    for raw in &raw_transactions {
        let notes: Vec<NoteHeader> = Deserializable::read_from_bytes(&raw.output_notes)?;
        all_note_ids.extend(notes.iter().map(NoteHeader::id));
        tx_output_notes.push(notes);
    }

    let mut output_notes_by_id = BTreeMap::new();
    for chunk in all_note_ids.chunks(QueryParamNoteCommitmentLimit::LIMIT) {
        output_notes_by_id.extend(select_note_sync_records(tx, chunk)?);
    }

    // Deserialize each transaction's input notes once and reuse them below. Authenticated inputs
    // have no header and carry only a nullifier, so gather those nullifiers to look their note IDs
    // up in one batch.
    let mut tx_input_notes: Vec<Vec<InputNoteCommitment>> =
        Vec::with_capacity(raw_transactions.len());
    let mut authenticated_nullifiers: Vec<Nullifier> = Vec::new();
    for raw in &raw_transactions {
        let commitments: Vec<InputNoteCommitment> =
            Deserializable::read_from_bytes(&raw.input_notes)?;
        for commitment in &commitments {
            if commitment.header().is_none() {
                authenticated_nullifiers.push(commitment.nullifier());
            }
        }
        tx_input_notes.push(commitments);
    }

    let mut note_ids_by_nullifier = BTreeMap::new();
    for chunk in authenticated_nullifiers.chunks(QueryParamNoteCommitmentLimit::LIMIT) {
        note_ids_by_nullifier.extend(select_note_ids_by_nullifier(tx, chunk)?);
    }

    // Assemble the final records.
    raw_transactions
        .into_iter()
        .zip(tx_output_notes)
        .zip(tx_input_notes)
        .map(|((raw, output_notes), input_notes)| {
            // Collect inclusion proofs for committed output notes. Notes not found in the `notes`
            // table were erased (created and consumed in the same batch).
            let output_note_proofs = output_notes
                .iter()
                .filter_map(|note| output_notes_by_id.get(&note.id()).cloned())
                .collect();

            // Build the side-channel refs. The input note commitments are left untouched, so the
            // header and its commitment stay exactly as the transaction submitted them.
            let consumed_note_refs = input_notes
                .iter()
                .filter(|commitment| commitment.header().is_none())
                .filter_map(|commitment| {
                    let nullifier = commitment.nullifier();
                    note_ids_by_nullifier.get(&nullifier).map(|note_id| (nullifier, *note_id))
                })
                .collect();

            let header = TransactionHeader::new_unchecked(
                raw.transaction_id,
                raw.account_id,
                raw.initial_state_commitment,
                raw.final_state_commitment,
                InputNotes::new_unchecked(input_notes),
                output_notes,
            );

            Ok(TransactionRecord {
                block_num: BlockNumber::from_raw_sql(raw.block_num)?,
                header,
                output_note_proofs,
                consumed_note_refs,
            })
        })
        .collect()
}
