use anyhow::{Context, ensure};
use miden_node_utils::tracing::miden_instrument;
use miden_protocol::block::{BlockNumber, BlockProof};
use miden_protocol::utils::serde::{
    BudgetedReader,
    ByteReader,
    Deserializable,
    SliceReader,
};

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
        proof_bytes: Vec<u8>,
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

        verify_block_proof(block_num, &proof_bytes)?;

        self.state.block_store.commit_proof(block_num, &proof_bytes).await?;
        self.state
            .proof_cache
            .push(block_num, ProofNotification::new(block_num, proof_bytes))
            .expect("proof cache receives sequential block numbers");
        self.state.proven_tip.advance(block_num);
        Ok(())
    }
}

/// Verifies that `proof_bytes` is a valid [`BlockProof`] for the block at `block_num`.
fn verify_block_proof(_block_num: BlockNumber, proof_bytes: &[u8]) -> anyhow::Result<()> {
    let _proof = decode_block_proof_exact(proof_bytes)?;

    // TODO: perform cryptographic verification once miden-protocol exposes a verifier.
    Ok(())
}

/// Decodes the exact serialized representation of a [`BlockProof`].
///
/// Exact decoding is important here because [`Deserializable::read_from_bytes`] intentionally
/// accepts trailing bytes. Proof bytes may originate from a remote prover or an upstream node, so
/// accepting an arbitrary suffix would persist and re-broadcast data that is not part of the proof.
fn decode_block_proof_exact(proof_bytes: &[u8]) -> anyhow::Result<BlockProof> {
    let reader = SliceReader::new(proof_bytes);
    let mut reader = BudgetedReader::new(reader, proof_bytes.len());
    let proof = BlockProof::read_from(&mut reader).context("failed to deserialize block proof")?;

    ensure!(!reader.has_more_bytes(), "block proof contains trailing bytes");
    Ok(proof)
}

#[cfg(test)]
mod tests {
    use miden_protocol::block::BlockProof;
    use miden_protocol::utils::serde::Serializable;

    use super::decode_block_proof_exact;

    #[test]
    fn accepts_canonical_block_proof_encoding() {
        let proof_bytes = BlockProof::new_dummy().to_bytes();

        decode_block_proof_exact(&proof_bytes).unwrap();
    }

    #[test]
    fn rejects_bytes_trailing_the_block_proof() {
        let mut proof_bytes = BlockProof::new_dummy().to_bytes();
        proof_bytes.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);

        let error = decode_block_proof_exact(&proof_bytes).unwrap_err();

        assert_eq!(error.to_string(), "block proof contains trailing bytes");
    }
}
