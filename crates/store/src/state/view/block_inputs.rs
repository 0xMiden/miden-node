//! Block input query for the block producer.
//!
//! Combines in-memory snapshot data (tree witnesses, partial MMR) with database lookups, scoping
//! the latter by the view's tip so both sources describe the same block height.

use std::collections::{BTreeMap, BTreeSet};

use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::block::account_tree::AccountWitness;
use miden_protocol::block::nullifier_tree::NullifierWitness;
use miden_protocol::block::{BlockInputs, BlockNumber};
use miden_protocol::crypto::merkle::mmr::PartialMmr;
use miden_protocol::note::Nullifier;
use miden_protocol::transaction::PartialBlockchain;

use super::StateView;
use crate::errors::GetBlockInputsError;

type BlockInputWitnesses = (
    BlockNumber,
    BTreeMap<AccountId, AccountWitness>,
    BTreeMap<Nullifier, NullifierWitness>,
    PartialMmr,
);

impl StateView {
    /// Returns data needed by the block producer to construct and prove the next block.
    pub async fn get_block_inputs(
        &self,
        account_ids: Vec<AccountId>,
        nullifiers: Vec<Nullifier>,
        unauthenticated_note_commitments: BTreeSet<Word>,
        reference_blocks: BTreeSet<BlockNumber>,
    ) -> Result<BlockInputs, GetBlockInputsError> {
        // Get the note inclusion proofs from the DB first: the reference blocks of the note proofs
        // are needed below to fetch their authentication paths in the chain MMR.
        let unauthenticated_note_proofs = self
            .db()
            .select_note_inclusion_proofs(unauthenticated_note_commitments)
            .await
            .map_err(GetBlockInputsError::SelectNoteInclusionProofError)?;

        // The set of blocks that the notes are included in.
        let note_proof_reference_blocks =
            unauthenticated_note_proofs.values().map(|proof| proof.location().block_num());

        // Collect all blocks we need to prove inclusion for, without duplicates.
        let mut blocks = reference_blocks;
        blocks.extend(note_proof_reference_blocks);

        let (latest_block_number, account_witnesses, nullifier_witnesses, partial_mmr) =
            self.get_block_inputs_witnesses(&mut blocks, &account_ids, &nullifiers)?;

        // Every block left in the set was validated against the latest block number by the witness
        // fetch above, and the latest block number is the view's tip itself.
        let scoped_blocks: Vec<_> = blocks
            .into_iter()
            .chain(std::iter::once(latest_block_number))
            .map(|block| self.scope_block(block).expect("blocks were validated against the tip"))
            .collect();

        // Fetch the block headers for all blocks in the partial MMR plus the latest one which will
        // be used as the previous block header of the block being built.
        let mut headers = self
            .db()
            .select_block_headers(scoped_blocks.into_iter())
            .await
            .map_err(GetBlockInputsError::SelectBlockHeaderError)?;

        // Find and remove the latest block as we must not add it to the chain MMR, since it is not
        // yet in the chain.
        let latest_block_header_index = headers
            .iter()
            .enumerate()
            .find_map(|(index, header)| {
                (header.block_num() == latest_block_number).then_some(index)
            })
            .expect("DB should have returned the header of the latest block header");

        // The order doesn't matter for PartialBlockchain::new, so swap remove is fine.
        let latest_block_header = headers.swap_remove(latest_block_header_index);

        // SAFETY: This should not error because:
        // - we're passing exactly the block headers that we've added to the partial MMR,
        // - so none of the block header's block numbers should exceed the chain length of the
        //   partial MMR,
        // - and we've added blocks to a BTreeSet, so there can be no duplicates.
        //
        // We construct headers and partial MMR in concert, so they are consistent. This is why we
        // can call the unchecked constructor.
        let partial_block_chain = PartialBlockchain::new_unchecked(partial_mmr, headers)
            .expect("partial mmr and block headers should be consistent");

        Ok(BlockInputs::new(
            latest_block_header,
            partial_block_chain,
            account_witnesses,
            nullifier_witnesses,
            unauthenticated_note_proofs,
        ))
    }

    /// Get account and nullifier witnesses for the requested account IDs and nullifier as well as
    /// the [`PartialMmr`] for the given blocks. The MMR won't contain the latest block and its
    /// number is removed from `blocks` and returned separately.
    fn get_block_inputs_witnesses(
        &self,
        blocks: &mut BTreeSet<BlockNumber>,
        account_ids: &[AccountId],
        nullifiers: &[Nullifier],
    ) -> Result<BlockInputWitnesses, GetBlockInputsError> {
        self.with_inner_read_blocking(|inner| {
            let latest_block_number = inner.latest_block_num();

            // If `blocks` is empty, use the latest block number which will never trigger the error.
            let highest_block_number = blocks.last().copied().unwrap_or(latest_block_number);
            if highest_block_number > latest_block_number {
                return Err(GetBlockInputsError::UnknownBatchBlockReference {
                    highest_block_number,
                    latest_block_number,
                });
            }

            // The latest block is not yet in the chain MMR, so we can't (and don't need to) prove
            // its inclusion in the chain.
            blocks.remove(&latest_block_number);

            // Fetch the partial MMR at the state of the latest block with authentication paths for
            // the provided set of blocks.
            //
            // SAFETY:
            // - The latest block num was retrieved from the inner blockchain from which we will
            //   also retrieve the proofs, so it is guaranteed to exist in that chain.
            // - We have checked that no block number in the blocks set is greater than latest block
            //   number *and* latest block num was removed from the set. Therefore only block
            //   numbers smaller than latest block num remain in the set. Therefore all the block
            //   numbers are guaranteed to exist in the chain state at latest block num.
            let partial_mmr =
                inner.blockchain.partial_mmr_from_blocks(blocks, latest_block_number).expect(
                    "latest block num should exist and all blocks in set should be < than latest block",
                );

            // Fetch witnesses for all accounts.
            let account_witnesses = account_ids
                .iter()
                .copied()
                .map(|account_id| (account_id, inner.account_tree.open_latest(account_id)))
                .collect::<BTreeMap<AccountId, AccountWitness>>();

            // Fetch witnesses for all nullifiers. We don't check whether the nullifiers are spent
            // or not as this is done as part of proposing the block.
            let nullifier_witnesses: BTreeMap<Nullifier, NullifierWitness> = nullifiers
                .iter()
                .copied()
                .map(|nullifier| (nullifier, inner.nullifier_tree.open(&nullifier)))
                .collect();

            Ok((latest_block_number, account_witnesses, nullifier_witnesses, partial_mmr))
        })
    }
}
