//! Helper functions for processing and validating transactions and batches.
//!
//! This module contains utilities for:
//! - Stripping decorators from output notes
//! - Rebuilding transactions and batches
//! - Validating network account restrictions

use std::sync::Arc;

use miden_objects::batch::ProvenBatch;
use miden_objects::note::{Note, NoteRecipient, NoteScript};
use miden_objects::transaction::{OutputNote, ProvenTransaction, ProvenTransactionBuilder};

// TRANSACTION PROCESSING
// ================================================================================================

/// Strips decorators from a single output note.
///
/// Decorators are removed from the note's script MAST (Merkle Abstract Syntax Tree)
/// to ensure consistency when submitting transactions to the network.
pub fn strip_note_decorators(note: &OutputNote) -> OutputNote {
    match note {
        OutputNote::Full(note) => {
            let mut mast = note.script().mast().clone();
            Arc::make_mut(&mut mast).strip_decorators();
            let script = NoteScript::from_parts(mast, note.script().entrypoint());
            let recipient = NoteRecipient::new(note.serial_num(), script, note.inputs().clone());
            let new_note = Note::new(note.assets().clone(), *note.metadata(), recipient);
            OutputNote::Full(new_note)
        },
        other => other.clone(),
    }
}

/// Rebuilds a transaction with decorators stripped from output notes.
///
/// This function creates a new `ProvenTransaction` from the original, but with
/// all decorators removed from the output notes' scripts.
pub fn rebuild_transaction_without_decorators(
    tx: &ProvenTransaction,
) -> Result<ProvenTransaction, String> {
    let mut builder = ProvenTransactionBuilder::new(
        tx.account_id(),
        tx.account_update().initial_state_commitment(),
        tx.account_update().final_state_commitment(),
        tx.account_update().account_delta_commitment(),
        tx.ref_block_num(),
        tx.ref_block_commitment(),
        tx.fee(),
        tx.expiration_block_num(),
        tx.proof().clone(),
    )
    .account_update_details(tx.account_update().details().clone())
    .add_input_notes(tx.input_notes().iter().cloned());

    let stripped_outputs = tx.output_notes().iter().map(strip_note_decorators);
    builder = builder.add_output_notes(stripped_outputs);

    builder.build().map_err(|e| e.to_string())
}

/// Rebuilds a batch with decorators stripped from output notes.
///
/// This function creates a new `ProvenBatch` from the original, but with
/// all decorators removed from the output notes' scripts.
pub fn rebuild_batch_without_decorators(batch: &ProvenBatch) -> Result<ProvenBatch, String> {
    let stripped_outputs: Vec<OutputNote> =
        batch.output_notes().iter().map(strip_note_decorators).collect();

    ProvenBatch::new(
        batch.id(),
        batch.reference_block_commitment(),
        batch.reference_block_num(),
        batch.account_updates().clone(),
        batch.input_notes().clone(),
        stripped_outputs,
        batch.batch_expiration_block_num(),
        batch.transactions().clone(),
    )
    .map_err(|e| e.to_string())
}

// VALIDATION HELPERS
// ================================================================================================

/// Validates that a transaction does not violate network account restrictions.
///
/// Network accounts cannot be used for user-submitted transactions that modify
/// their initial state. Only deployment transactions for new network accounts are allowed.
pub fn validate_network_account_restriction(tx: &ProvenTransaction) -> Result<(), &'static str> {
    if tx.account_id().is_network() && !tx.account_update().initial_state_commitment().is_empty() {
        return Err("Network transactions may not be submitted by users yet");
    }
    Ok(())
}

/// Validates that all transactions in a batch do not violate network account restrictions.
///
/// See [`validate_network_account_restriction`] for details on the restriction.
pub fn validate_batch_network_account_restrictions(
    batch: &ProvenBatch,
) -> Result<(), &'static str> {
    for tx in batch.transactions().as_slice() {
        if tx.account_id().is_network() && !tx.initial_state_commitment().is_empty() {
            return Err("Network transactions may not be submitted by users yet");
        }
    }
    Ok(())
}
