use miden_node_store::{DatabaseError, Db};
use miden_protocol::block::{BlockNumber, BlockSigner, ProposedBlock};
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::Signature;
use miden_protocol::errors::ProposedBlockError;
use miden_protocol::transaction::TransactionId;
use tracing::info_span;

use crate::db::select_transactions;

// BLOCK VALIDATION ERROR
// ================================================================================================

#[derive(thiserror::Error, Debug)]
pub enum BlockValidationError {
    #[error("transaction {0} in block {1} has not been validated")]
    TransactionNotValidated(TransactionId, BlockNumber),
    #[error("failed to build block")]
    BlockBuildingFailed(#[from] ProposedBlockError),
    #[error("failed to select transactions")]
    DatabaseError(#[from] DatabaseError),
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
    db: &Db,
) -> Result<Signature, BlockValidationError> {
    let verify_span = info_span!("verify_transactions");

    // Retrieve all validated transactions pertaining to the proposed block.
    let tx_ids = proposed_block.transactions().map(|tx| tx.id()).collect::<Vec<_>>();
    let validated_transactions = db
        .transact("select_transactions", move |conn| select_transactions(conn, &tx_ids))
        .await?;

    // Build the block header.
    let (header, _) = proposed_block.into_header_and_body()?;

    // Sign the header.
    let signature = info_span!("sign_block").in_scope(|| signer.sign(&header));

    Ok(signature)
}
