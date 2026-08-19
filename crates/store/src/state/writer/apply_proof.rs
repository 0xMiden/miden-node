use anyhow::ensure;
use miden_node_utils::tracing::miden_instrument;
use miden_protocol::block::{BlockNumber, BlockProof};
use miden_protocol::utils::serde::Serializable;

use crate::COMPONENT;
use crate::state::{ProofNotification, ProofWriter};

impl ProofWriter {
    /// Saves a block proof, advances the proven-in-sequence tip, and notifies replica subscribers.
    ///
    /// # Errors
    ///
    /// - If proofs are not applied in strict ascending order (exactly one block past the proven tip)
    /// - If the proof's corresponding block was not already committed
    #[miden_instrument(
        target = COMPONENT,
        err,
        fields(
            block.number = block_num.as_u32(),
        ),
    )]
    pub async fn apply_proof(
        &mut self,
        block_num: BlockNumber,
        proof: BlockProof,
    ) -> anyhow::Result<()> {
        let expected = self.state.proven_tip().child();
        ensure!(
            block_num == expected,
            "out-of-sequence proof: expected block {expected}, got {block_num}",
        );

        let committed_tip = self.state.committed_tip();
        ensure!(
            block_num <= committed_tip,
            "proof for uncommitted block {block_num} exceeds committed tip {committed_tip}",
        );

        verify_block_proof(block_num, &proof)?;

        // Persistence remains in the canonical Miden protocol encoding. The gRPC boundary uses a
        // structured proof message and passes the domain value to this writer.
        let proof_bytes = proof.to_bytes();

        self.state.block_store.commit_proof(block_num, &proof_bytes).await?;
        self.state
            .proof_cache
            .push(block_num, ProofNotification::new(block_num, proof_bytes))
            .expect("proof cache receives sequential block numbers");
        self.state.proven_tip.advance(block_num);
        Ok(())
    }
}

/// Verifies that `proof` is a valid [`BlockProof`] for the block at `block_num`.
fn verify_block_proof(_block_num: BlockNumber, proof: &BlockProof) -> anyhow::Result<()> {
    // TODO: perform verification.
    ensure!(
        proof.to_bytes().is_empty(),
        "unsupported non-empty placeholder block proof encoding"
    );
    Ok(())
}
