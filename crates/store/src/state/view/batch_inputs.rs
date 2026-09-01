//! Batch input query for the block producer.
//!
//! Combines in-memory snapshot data (partial MMR) with database lookups, scoping the latter by
//! the view's tip so both sources describe the same block height.

use std::collections::BTreeSet;

use miden_node_proto::domain::batch::BatchInputs;
use miden_protocol::Word;
use miden_protocol::block::BlockNumber;
use miden_protocol::transaction::PartialBlockchain;

use super::StateView;
use crate::errors::GetBatchInputsError;

impl StateView {
    /// Fetches the inputs for a transaction batch from the database.
    ///
    /// ## Inputs
    ///
    /// The function takes as input:
    /// - The batch reference block is the block against which the batch is built.
    /// - The tx reference blocks are the set of blocks referenced by transactions in the batch.
    /// - The unauthenticated note commitments are the set of commitments of unauthenticated notes
    ///   consumed by all transactions in the batch. For these notes, we attempt to find inclusion
    ///   proofs. Not all notes will exist in the DB necessarily, as some notes can be created and
    ///   consumed within the same batch.
    ///
    /// ## Outputs
    ///
    /// The function will return:
    /// - A block inclusion proof for all tx reference blocks and for all blocks which are
    ///   referenced by a note inclusion proof.
    /// - Note inclusion proofs for all notes that were found in the DB.
    /// - The block header that the batch references.
    pub async fn get_batch_inputs(
        &self,
        batch_reference_block_num: BlockNumber,
        tx_reference_blocks: BTreeSet<BlockNumber>,
        unauthenticated_note_commitments: BTreeSet<Word>,
    ) -> Result<BatchInputs, GetBatchInputsError> {
        if tx_reference_blocks.is_empty() {
            return Err(GetBatchInputsError::TransactionBlockReferencesEmpty);
        }

        let latest_block_num = self.tip();
        let batch_reference_block_num = self.scope_block(batch_reference_block_num).ok_or(
            GetBatchInputsError::UnknownBatchReferenceBlock {
                reference_block_num: batch_reference_block_num,
                latest_block_num: *latest_block_num,
            },
        )?;

        // First we grab note inclusion proofs for the known notes. These proofs only prove that the
        // note was included in a given block. We then also need to prove that each of those blocks
        // is included in the chain. The proofs are scoped by the view's tip, so the database cannot
        // report a note from a block the pinned snapshot cannot prove yet.
        let note_proofs = self
            .db
            .select_note_inclusion_proofs(
                unauthenticated_note_commitments,
                batch_reference_block_num,
            )
            .await
            .map_err(GetBatchInputsError::SelectNoteInclusionProofError)?;

        // The set of blocks that the notes are included in.
        let note_blocks = note_proofs.values().map(|proof| proof.location().block_num());

        // Collect all blocks we need to query without duplicates, which is:
        // - all blocks for which we need to prove note inclusion.
        // - all blocks referenced by transactions in the batch.
        let mut blocks: BTreeSet<BlockNumber> = tx_reference_blocks;
        blocks.extend(note_blocks);

        // Remove the batch reference block from the tracked blocks. The batch kernel adds this
        // block to the MMR, so the partial blockchain does not contain it.
        blocks.remove(&*batch_reference_block_num);

        // Batch validation ensures that all transaction reference blocks are at or below the batch
        // reference block. The note query applies the same limit to note blocks.
        let scoped_blocks = blocks
            .iter()
            .map(|&block| {
                self.scope_block(block)
                    .expect("batch input blocks must not exceed the batch reference block")
            })
            .collect::<Vec<_>>();

        // SAFETY:
        // - The batch reference block was scoped against the view's blockchain.
        // - The batch reference block was removed from the set.
        // - All remaining block numbers are less than the batch reference block.
        let partial_mmr = self
            .blockchain()
            .partial_mmr_from_blocks(&blocks, *batch_reference_block_num)
            .expect("all tracked blocks must exist before the batch reference block");

        // Fetch the reference block of the batch as part of this query, so we can avoid looking it
        // up in a separate DB access.
        let mut headers = self
            .db
            .select_block_headers(
                scoped_blocks.into_iter().chain(std::iter::once(batch_reference_block_num)),
            )
            .await
            .map_err(GetBatchInputsError::SelectBlockHeaderError)?;

        // Find and remove the batch reference block as we don't want to add it to the chain MMR.
        let header_index = headers
            .iter()
            .enumerate()
            .find_map(|(index, header)| {
                (header.block_num() == *batch_reference_block_num).then_some(index)
            })
            .expect("DB should have returned the header of the batch reference block");

        // The order doesn't matter for PartialBlockchain::new, so swap remove is fine.
        let batch_reference_block_header = headers.swap_remove(header_index);

        // SAFETY: This should not error because:
        // - we're passing exactly the block headers that we've added to the partial MMR,
        // - so none of the block headers block numbers should exceed the chain length of the
        //   partial MMR,
        // - and we've added blocks to a BTreeSet, so there can be no duplicates.
        //
        // We construct headers and partial MMR in concert, so they are consistent. This is why we
        // can call the unchecked constructor.
        let partial_block_chain = PartialBlockchain::new_unchecked(partial_mmr, headers)
            .expect("partial mmr and block headers should be consistent");

        Ok(BatchInputs {
            batch_reference_block_header,
            note_proofs,
            partial_block_chain,
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
    async fn batch_inputs_use_an_explicit_reference_block() {
        let data_directory = tempfile::tempdir().expect("tempdir should be created");
        bootstrap_store(data_directory.path());
        let (state, _block_writer, _proof_writer) = State::for_tests(data_directory.path()).await;

        let inputs = state
            .view()
            .get_batch_inputs(
                BlockNumber::GENESIS,
                BTreeSet::from([BlockNumber::GENESIS]),
                BTreeSet::new(),
            )
            .await
            .expect("batch inputs should be returned");
        assert_eq!(inputs.batch_reference_block_header.block_num(), BlockNumber::GENESIS);
        assert_eq!(inputs.partial_block_chain.chain_length(), BlockNumber::GENESIS);

        let error = state
            .view()
            .get_batch_inputs(
                BlockNumber::GENESIS.child(),
                BTreeSet::from([BlockNumber::GENESIS]),
                BTreeSet::new(),
            )
            .await
            .expect_err("a batch reference after the tip should fail");
        assert!(matches!(
            error,
            GetBatchInputsError::UnknownBatchReferenceBlock {
                reference_block_num,
                latest_block_num,
            } if reference_block_num == BlockNumber::GENESIS.child()
                && latest_block_num == BlockNumber::GENESIS
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
