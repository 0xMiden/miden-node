//! Note-related queries.

use miden_node_db::sqlite::{ReadTx, WriteTx};
use miden_node_db::{DatabaseError, SqlTypeConvert};
use miden_node_utils::ErrorReport;
use miden_protocol::account::AccountId;
use miden_protocol::block::BlockNumber;
use miden_protocol::note::{Note, NoteId, Nullifier};
use miden_standards::note::AccountTargetNetworkNote;

use crate::NoteError;
use crate::db::sql;

/// Row returned by [`get_note_status`].
#[derive(Debug, Clone)]
pub struct NoteStatusRow {
    pub last_error: Option<String>,
    pub attempt_count: i64,
    pub last_attempt: Option<i64>,
    pub committed_at: Option<i64>,
}

/// Inserts network notes from a committed block. Uses `INSERT OR IGNORE` so re-applying the same
/// block (e.g. on a redelivery from the subscription stream) is a no-op rather than a constraint
/// violation.
pub fn insert_network_notes(
    tx: &WriteTx<'_>,
    notes: &[AccountTargetNetworkNote],
) -> Result<(), DatabaseError> {
    for note in notes {
        let inner = note.as_note();
        tx.execute(
            sql::INSERT_NETWORK_NOTE,
            &[&inner.nullifier(), &note.target_account_id(), inner, &inner.id()],
        )?;
    }
    Ok(())
}

/// Marks notes as consumed by setting `committed_at` to the block number whose committed body
/// contained their nullifier. Rows for nullifiers we never inserted (notes whose targets are not
/// network accounts, or notes that arrived before our subscription cursor) are silently skipped.
///
/// Rows are kept around (not deleted) so the `GetNetworkNoteStatus` endpoint can report the full
/// lifecycle of any note the ntx-builder has ever seen.
pub fn mark_notes_consumed(
    tx: &WriteTx<'_>,
    nullifiers: &[Nullifier],
    block_num: BlockNumber,
) -> Result<(), DatabaseError> {
    let block_num_val = block_num.to_raw_sql();
    for nullifier in nullifiers {
        tx.execute(sql::MARK_NOTE_CONSUMED, &[nullifier, &block_num_val])?;
    }
    Ok(())
}

/// Returns `true` if there is at least one note available for consumption by the given account.
pub fn has_available_notes(
    tx: &ReadTx<'_>,
    account_id: AccountId,
    block_num: BlockNumber,
    max_attempts: usize,
) -> Result<bool, DatabaseError> {
    Ok(!available_notes(tx, account_id, block_num, max_attempts)?.is_empty())
}

/// Returns notes available for consumption by a given account.
///
/// Selects unconsumed notes for the account (a row exists only while a note is unconsumed) whose
/// `attempt_count` is below the cap, then applies execution-hint and backoff filtering in Rust.
#[expect(clippy::cast_possible_wrap)]
pub fn available_notes(
    tx: &ReadTx<'_>,
    account_id: AccountId,
    block_num: BlockNumber,
    max_attempts: usize,
) -> Result<Vec<AccountTargetNetworkNote>, DatabaseError> {
    let rows = tx.query(sql::AVAILABLE_NOTES, &[&account_id, &(max_attempts as i64)], |row| {
        Ok((row.get::<Note>(0)?, row.get::<i64>(1)?, row.get::<Option<i64>>(2)?))
    })?;

    let mut result = Vec::new();
    for (note, attempt_count, last_attempt) in rows {
        #[expect(clippy::cast_sign_loss)]
        let attempt_count = attempt_count as usize;
        let last_attempt = last_attempt.map(BlockNumber::from_raw_sql).transpose()?;
        let note = AccountTargetNetworkNote::new(note).map_err(|source| {
            DatabaseError::deserialization("failed to convert to network note", source)
        })?;

        let execution_hint_ok = note.execution_hint().can_be_consumed(block_num).unwrap_or(true);
        if execution_hint_ok && has_backoff_passed(block_num, last_attempt, attempt_count) {
            result.push(note);
        }
    }

    Ok(result)
}

/// Marks notes as failed by incrementing `attempt_count`, setting `last_attempt`, and storing the
/// latest error message.
pub fn notes_failed(
    tx: &WriteTx<'_>,
    failed_notes: &[(Nullifier, NoteError)],
    block_num: BlockNumber,
) -> Result<(), DatabaseError> {
    let block_num_val = block_num.to_raw_sql();

    for (nullifier, error) in failed_notes {
        let error_report = error.as_report();
        tx.execute(sql::NOTE_FAILED, &[nullifier, &block_num_val, &error_report])?;
    }
    Ok(())
}

/// Marks notes as permanently unconsumable by pinning `attempt_count` to `max_attempts`.
///
/// A note whose own consumption exceeds the per-transaction cycle budget can never be consumed in
/// any transaction, so retrying it is pointless. Setting `attempt_count` to `max_attempts` takes it
/// out of the pending set immediately (`available_notes`/`account_has_pending_notes` filter on
/// `attempt_count < max_attempts`) and makes [`get_note_status`] derive it as `Discarded`, while
/// `last_error` records why.
#[expect(clippy::cast_possible_wrap)]
pub fn discard_notes(
    tx: &WriteTx<'_>,
    nullifiers: &[Nullifier],
    block_num: BlockNumber,
    max_attempts: usize,
    reason: &str,
) -> Result<(), DatabaseError> {
    let block_num_val = block_num.to_raw_sql();
    let reason = reason.to_string();
    for nullifier in nullifiers {
        tx.execute(
            sql::DISCARD_NOTE,
            &[nullifier, &(max_attempts as i64), &block_num_val, &reason],
        )?;
    }
    Ok(())
}

/// Returns the status for a note identified by its note ID.
pub fn get_note_status(
    tx: &ReadTx<'_>,
    note_id: NoteId,
) -> Result<Option<NoteStatusRow>, DatabaseError> {
    Ok(tx
        .query(sql::GET_NOTE_STATUS, &[&note_id], |row| {
            Ok(NoteStatusRow {
                last_error: row.get::<Option<String>>(0)?,
                attempt_count: row.get::<i64>(1)?,
                last_attempt: row.get::<Option<i64>>(2)?,
                committed_at: row.get::<Option<i64>>(3)?,
            })
        })?
        .into_iter()
        .next())
}

/// Returns the distinct set of network accounts that currently have at least one pending note
/// (unconsumed and within the per-note attempt budget).
#[expect(clippy::cast_possible_wrap)]
pub fn accounts_with_pending_notes(
    tx: &ReadTx<'_>,
    max_attempts: usize,
) -> Result<Vec<AccountId>, DatabaseError> {
    tx.query(sql::ACCOUNTS_WITH_PENDING_NOTES, &[&(max_attempts as i64)], |row| {
        row.get::<AccountId>(0)
    })
}

// HELPERS
// ================================================================================================

/// Checks if the backoff block period has passed.
///
/// The number of blocks passed since the last attempt must be greater than or equal to
/// e^(0.25 * `attempt_count`) rounded to the nearest integer.
#[expect(clippy::cast_precision_loss, clippy::cast_sign_loss)]
fn has_backoff_passed(
    chain_tip: BlockNumber,
    last_attempt: Option<BlockNumber>,
    attempts: usize,
) -> bool {
    if attempts == 0 {
        return true;
    }
    let blocks_passed = last_attempt
        .and_then(|last| chain_tip.checked_sub(last.as_u32()))
        .unwrap_or_default();

    let backoff_threshold = (0.25 * attempts as f64).exp().round() as usize;

    blocks_passed.as_usize() > backoff_threshold
}

#[cfg(test)]
mod tests {
    use miden_protocol::block::BlockNumber;

    use super::has_backoff_passed;

    #[rstest::rstest]
    #[test]
    #[case::all_zero(Some(BlockNumber::GENESIS), BlockNumber::GENESIS, 0, true)]
    #[case::no_attempts(None, BlockNumber::GENESIS, 0, true)]
    #[case::one_attempt(Some(BlockNumber::GENESIS), BlockNumber::from(2), 1, true)]
    #[case::three_attempts(Some(BlockNumber::GENESIS), BlockNumber::from(3), 3, true)]
    #[case::ten_attempts(Some(BlockNumber::GENESIS), BlockNumber::from(13), 10, true)]
    #[case::twenty_attempts(Some(BlockNumber::GENESIS), BlockNumber::from(149), 20, true)]
    #[case::one_attempt_false(Some(BlockNumber::GENESIS), BlockNumber::from(1), 1, false)]
    #[case::three_attempts_false(Some(BlockNumber::GENESIS), BlockNumber::from(2), 3, false)]
    #[case::ten_attempts_false(Some(BlockNumber::GENESIS), BlockNumber::from(12), 10, false)]
    #[case::twenty_attempts_false(Some(BlockNumber::GENESIS), BlockNumber::from(148), 20, false)]
    fn backoff_has_passed(
        #[case] last_attempt_block_num: Option<BlockNumber>,
        #[case] current_block_num: BlockNumber,
        #[case] attempt_count: usize,
        #[case] backoff_should_have_passed: bool,
    ) {
        assert_eq!(
            backoff_should_have_passed,
            has_backoff_passed(current_block_num, last_attempt_block_num, attempt_count)
        );
    }
}
