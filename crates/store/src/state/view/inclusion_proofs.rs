//! Inclusion proof query.
//!
//! Combines in-memory snapshot data (partial MMR) with database lookups, scoping the latter by
//! the view's tip so both sources describe the same block height.

use std::collections::{BTreeMap, BTreeSet};

use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::note::{NoteId, NoteInclusionProof};
use miden_protocol::transaction::PartialBlockchain;

use super::StateView;
use crate::errors::GetInclusionProofsError;

/// Block and note inclusion proofs relative to a reference block.
#[derive(Clone, Debug)]
pub struct InclusionProofs {
    pub reference_block_header: BlockHeader,
    pub partial_blockchain: PartialBlockchain,
    pub note_inclusion_proofs: BTreeMap<NoteId, NoteInclusionProof>,
}

impl StateView {
    /// Fetches block and note inclusion proofs relative to the reference block.
    ///
    /// ## Inputs
    ///
    /// The function takes these inputs:
    /// - The reference block identifies the requested header and blockchain state.
    /// - The block numbers identify the blocks to include in the partial blockchain. The reference
    ///   block header is returned separately.
    /// - The note IDs identify the notes for which to find inclusion proofs.
    ///
    /// ## Outputs
    ///
    /// The function returns:
    /// - The reference block header.
    /// - A partial blockchain for all earlier requested blocks and all blocks used by the note
    ///   proofs.
    /// - Inclusion proofs for all requested notes that the database contains.
    pub async fn get_inclusion_proofs(
        &self,
        reference_block: BlockNumber,
        block_numbers: BTreeSet<BlockNumber>,
        note_ids: BTreeSet<NoteId>,
    ) -> Result<InclusionProofs, GetInclusionProofsError> {
        let latest_block_num = self.tip();
        let reference_block = self.scope_block(reference_block).ok_or(
            GetInclusionProofsError::ReferenceBlockAfterTip {
                reference_block,
                latest_block_num: *latest_block_num,
            },
        )?;

        if let Some(&block_num) = block_numbers.last()
            && block_num > *reference_block
        {
            return Err(GetInclusionProofsError::BlockAfterReferenceBlock {
                block_num,
                reference_block: *reference_block,
            });
        }

        // Fetch the note inclusion proofs first. Each proof identifies another block that the
        // partial blockchain must authenticate. The block limit prevents the database from
        // returning a note from a later block.
        let note_commitments = note_ids.into_iter().map(|note_id| note_id.as_word()).collect();
        let note_inclusion_proofs = self
            .db
            .select_note_inclusion_proofs(note_commitments, reference_block)
            .await
            .map_err(GetInclusionProofsError::SelectNoteInclusionProofError)?;

        // The set of blocks that the notes are included in.
        let note_blocks = note_inclusion_proofs.values().map(|proof| proof.location().block_num());

        // Collect all blocks we need to query without duplicates, which is:
        // - all blocks for which we need to prove note inclusion.
        // - all requested blocks.
        let mut blocks = block_numbers;
        blocks.extend(note_blocks);

        // The partial blockchain describes the chain state before the reference block.
        blocks.remove(&*reference_block);

        // All blocks are at or below the reference block and the view tip.
        let scoped_blocks = blocks
            .iter()
            .map(|&block| {
                self.scope_block(block)
                    .expect("requested blocks must not exceed the reference block")
            })
            .collect::<Vec<_>>();

        // SAFETY:
        // - The reference block was scoped against the view's blockchain.
        // - The reference block was removed from the set.
        // - All remaining block numbers are less than the reference block.
        let partial_mmr = self
            .blockchain()
            .partial_mmr_from_blocks(&blocks, *reference_block)
            .expect("all requested blocks must exist before the reference block");

        // Fetch the reference block header in the same database query as the requested headers.
        let mut headers = self
            .db
            .select_block_headers(scoped_blocks.into_iter().chain(std::iter::once(reference_block)))
            .await
            .map_err(GetInclusionProofsError::SelectBlockHeaderError)?;

        // Remove the reference block header because the partial blockchain does not track it.
        let header_index = headers
            .iter()
            .enumerate()
            .find_map(|(index, header)| (header.block_num() == *reference_block).then_some(index))
            .expect("DB should have returned the reference block header");

        // PartialBlockchain::new does not require ordered headers.
        let reference_block_header = headers.swap_remove(header_index);

        // SAFETY:
        // - The headers match the blocks in the partial MMR.
        // - No header exceeds the chain length of the partial MMR.
        // - The BTreeSet removes duplicate block numbers.
        //
        // The headers and the partial MMR use the same block set. The unchecked constructor is safe.
        let partial_blockchain = PartialBlockchain::new_unchecked(partial_mmr, headers)
            .expect("partial mmr and block headers should be consistent");

        Ok(InclusionProofs {
            reference_block_header,
            partial_blockchain,
            note_inclusion_proofs,
        })
    }
}

#[cfg(test)]
mod tests {
    use miden_node_utils::fee::test_fee_params;
    use miden_protocol::block::ValidatorKeys;
    use miden_protocol::testing::random_secret_key::random_secret_key;

    use super::*;
    use crate::GenesisState;
    use crate::state::State;

    #[tokio::test]
    async fn inclusion_proofs_use_the_requested_block() {
        let data_directory = tempfile::tempdir().expect("tempdir should be created");
        bootstrap_store(data_directory.path());
        let (state, _block_writer, _proof_writer) = State::for_tests(data_directory.path()).await;

        let inputs = state
            .view()
            .get_inclusion_proofs(BlockNumber::GENESIS, BTreeSet::new(), BTreeSet::new())
            .await
            .expect("inclusion proofs should be returned");
        assert_eq!(inputs.reference_block_header.block_num(), BlockNumber::GENESIS);
        assert_eq!(inputs.partial_blockchain.chain_length(), BlockNumber::GENESIS);

        let error = state
            .view()
            .get_inclusion_proofs(
                BlockNumber::GENESIS.child(),
                BTreeSet::from([BlockNumber::GENESIS]),
                BTreeSet::new(),
            )
            .await
            .expect_err("a reference block after the tip should fail");
        assert!(matches!(
            error,
            GetInclusionProofsError::ReferenceBlockAfterTip {
                reference_block,
                latest_block_num,
            } if reference_block == BlockNumber::GENESIS.child()
                && latest_block_num == BlockNumber::GENESIS
        ));

        let error = state
            .view()
            .get_inclusion_proofs(
                BlockNumber::GENESIS,
                BTreeSet::from([BlockNumber::GENESIS.child()]),
                BTreeSet::new(),
            )
            .await
            .expect_err("a requested block after the reference block should fail");
        assert!(matches!(
            error,
            GetInclusionProofsError::BlockAfterReferenceBlock {
                block_num,
                reference_block,
            } if block_num == BlockNumber::GENESIS.child()
                && reference_block == BlockNumber::GENESIS
        ));
    }

    fn bootstrap_store(path: &std::path::Path) {
        let signer = random_secret_key();
        let genesis_state = GenesisState::new(
            vec![],
            test_fee_params(),
            1,
            1,
            ValidatorKeys::new(vec![signer.public_key()]).expect("validator keys should be valid"),
        );
        let genesis_block = genesis_state.into_block().expect("genesis block should be created");

        State::bootstrap(genesis_block, path).expect("store should bootstrap");
    }
}
