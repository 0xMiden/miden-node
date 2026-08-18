//! Marks notes as permanently unconsumable.

use miden_node_db::sqlite::WriteTx;
use miden_node_db::{DatabaseError, SqlTypeConvert};
use miden_protocol::block::BlockNumber;
use miden_protocol::note::Nullifier;

const SQL: &str = include_str!("discard_note.sql");

/// Marks notes as permanently unconsumable by pinning `attempt_count` to `max_attempts`.
///
/// A note whose own consumption exceeds the per-transaction cycle budget can never be consumed in
/// any transaction, so retrying it is pointless. Setting `attempt_count` to `max_attempts` takes it
/// out of the pending set immediately (`available_notes`/`account_has_pending_notes` filter on
/// `attempt_count < max_attempts`) and makes
/// [`get_note_status`](super::get_note_status) derive it as `Discarded`, while `last_error` records
/// why.
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
            SQL,
            &[nullifier, &(max_attempts as i64), &block_num_val, &reason],
        )?;
    }
    Ok(())
}
