//! Inserts network notes from a committed block.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::WriteTx;
use miden_standards::note::AccountTargetNetworkNote;

const SQL: &str = include_str!("insert_network_note.sql");

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
            SQL,
            &[
                &inner.nullifier(),
                &note.target_account_id(),
                inner,
                &inner.id(),
            ],
        )?;
    }
    Ok(())
}
