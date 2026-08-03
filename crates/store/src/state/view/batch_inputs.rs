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
    /// - The block header that the batch should reference, i.e. the latest known block.
    pub async fn get_batch_inputs(
        &self,
        tx_reference_blocks: BTreeSet<BlockNumber>,
        unauthenticated_note_commitments: BTreeSet<Word>,
    ) -> Result<BatchInputs, GetBatchInputsError> {
        if tx_reference_blocks.is_empty() {
            return Err(GetBatchInputsError::TransactionBlockReferencesEmpty);
        }

        // First we grab note inclusion proofs for the known notes. These proofs only prove that the
        // note was included in a given block. We then also need to prove that each of those blocks
        // is included in the chain.
        let note_proofs = self
            .db()
            .select_note_inclusion_proofs(unauthenticated_note_commitments)
            .await
            .map_err(GetBatchInputsError::SelectNoteInclusionProofError)?;

        // The set of blocks that the notes are included in.
        let note_blocks = note_proofs.values().map(|proof| proof.location().block_num());

        // Collect all blocks we need to query without duplicates, which is:
        // - all blocks for which we need to prove note inclusion.
        // - all blocks referenced by transactions in the batch.
        let mut blocks: BTreeSet<BlockNumber> = tx_reference_blocks;
        blocks.extend(note_blocks);

        let batch_reference_block = self.tip();

        // Remove the latest block from the to-be-tracked blocks as it will be the reference block
        // for the batch itself and thus added to the MMR within the batch kernel, so there is no
        // need to prove its inclusion.
        blocks.remove(&batch_reference_block);

        // Scoping the blocks doubles as the validation that none lies beyond the view's tip. Scoped
        // in descending order, so the first failure carries the highest block number.
        let scoped_blocks = blocks
            .iter()
            .rev()
            .map(|&block| {
                self.scope_block(block).ok_or(
                    GetBatchInputsError::UnknownTransactionBlockReference {
                        highest_block_num: block,
                        latest_block_num: *batch_reference_block,
                    },
                )
            })
            .collect::<Result<Vec<_>, _>>()?;

        // SAFETY:
        // - The latest block num was retrieved from the view's blockchain from which we will
        //   also retrieve the proofs, so it is guaranteed to exist in that chain.
        // - Scoping above proved that no block in the set is greater than the latest block number
        //   *and* the latest block num was removed from the set. Therefore only block numbers
        //   smaller than latest block num remain in the set. Therefore all the block numbers are
        //   guaranteed to exist in the chain state at latest block num.
        let partial_mmr =
            self.blockchain().partial_mmr_from_blocks(&blocks, *batch_reference_block).expect(
                "latest block num should exist and all blocks in set should be < than latest block",
            );

        // Fetch the reference block of the batch as part of this query, so we can avoid looking it
        // up in a separate DB access.
        let mut headers = self
            .db()
            .select_block_headers(
                scoped_blocks.into_iter().chain(std::iter::once(batch_reference_block)),
            )
            .await
            .map_err(GetBatchInputsError::SelectBlockHeaderError)?;

        // Find and remove the batch reference block as we don't want to add it to the chain MMR.
        let header_index = headers
            .iter()
            .enumerate()
            .find_map(|(index, header)| {
                (header.block_num() == *batch_reference_block).then_some(index)
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
