use std::sync::Arc;

use miden_lib::block::build_block;
use miden_objects::ProposedBlockError;
use miden_objects::block::{BlockNumber, BlockSigner, ProposedBlock};
use miden_objects::crypto::dsa::ecdsa_k256_keccak::Signature;
use miden_objects::transaction::TransactionId;

use crate::server::ValidatedTransactions;

// BLOCK VALIDATION ERROR
// ================================================================================================

#[derive(thiserror::Error, Debug)]
pub enum BlockValidationError {
    #[error("transaction {0} in block {1} has not been validated")]
    TransactionNotValidated(TransactionId, BlockNumber),
    #[error("failed to build block")]
    BlockBuildingFailed(#[from] ProposedBlockError),
}

// BLOCK VALIDATION
// ================================================================================================

/// Validates a block by checking that all transactions in the proposed block have been processed by
/// the validator in the past.
///
/// Removes the validated transactions from the cache upon success.
pub async fn validate_block<S: BlockSigner>(
    proposed_block: ProposedBlock,
    signer: &S,
    validated_transactions: Arc<ValidatedTransactions>,
) -> Result<Signature, BlockValidationError> {
    // Build the block.
    let (header, body) = build_block(proposed_block)?;

    // Check that all transactions in the proposed block have been validated
    let validated_txs = validated_transactions.read().await;
    for tx_header in body.transactions().as_slice() {
        let tx_id = tx_header.id();
        if !validated_txs.contains_key(&tx_id) {
            return Err(BlockValidationError::TransactionNotValidated(tx_id, header.block_num()));
        }
    }
    // Release the validated transactions read lock.
    drop(validated_txs);

    // Sign the header.
    let signature = signer.sign(&header);

    // Remove the validated transactions from the cache.
    let mut validated_txs = validated_transactions.write().await;
    for tx_header in body.transactions().as_slice() {
        validated_txs.remove(&tx_header.id());
    }
    // Release the validated transactions write lock.
    drop(validated_txs);

    Ok(signature)
}
