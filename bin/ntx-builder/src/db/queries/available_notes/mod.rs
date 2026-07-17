//! Selects notes available for consumption by a network account.

use miden_node_db::sqlite::ReadTx;
use miden_node_db::{DatabaseError, SqlTypeConvert};
use miden_protocol::account::AccountId;
use miden_protocol::block::BlockNumber;
use miden_protocol::note::Note;
use miden_standards::note::AccountTargetNetworkNote;

const SQL: &str = include_str!("available_notes.sql");

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
    let rows = tx.query(SQL, &[&account_id, &(max_attempts as i64)], |row| {
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
