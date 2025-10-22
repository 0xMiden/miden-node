use diesel::SqliteConnection;
use miden_objects::block::{ProvenBlock, SignedBlock};
use miden_objects::crypto::dsa::ecdsa_k256_keccak::SecretKey;
use tracing::instrument;

use crate::COMPONENT;

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("")]
    InvalidBlock,
    #[error("failed to sign block")]
    Signature(#[from] SigningError),
}

#[instrument(
        target = COMPONENT,
        name = "validator.validate_block",
        skip_all,
    )]
pub fn validate_block(
    conn: &mut SqliteConnection,
    proven_block: &ProvenBlock,
    secret_key: SecretKey,
) -> Result<SignedBlock, ValidationError> {
    // TODO: impl logic, retrieve relevant txs from db.
    let signed_block = proven_block.sign(secret_key);
    Ok(signed_block)
}
