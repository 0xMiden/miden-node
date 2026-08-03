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

use super::{ScopedBlockNum, StateView};
use crate::errors::GetBlockInputsError;

type BlockInputWitnesses = (
    ScopedBlockNum,
    Vec<ScopedBlockNum>,
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

        let (latest, blocks, account_witnesses, nullifier_witnesses, partial_mmr) =
            self.get_block_inputs_witnesses(blocks, &account_ids, &nullifiers)?;

        // Fetch the block headers for all blocks in the partial MMR plus the latest one which will
        // be used as the previous block header of the block being built.
        let mut headers = self
            .db()
            .select_block_headers(blocks.into_iter().chain(std::iter::once(latest)))
            .await
            .map_err(GetBlockInputsError::SelectBlockHeaderError)?;

        // Find and remove the latest block as we must not add it to the chain MMR, since it is not
        // yet in the chain.
        let latest_block_header_index = headers
            .iter()
            .enumerate()
            .find_map(|(index, header)| (header.block_num() == *latest).then_some(index))
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

    /// Get account and nullifier witnesses for the requested account IDs and nullifiers, the
    /// [`PartialMmr`] for the given blocks, and the blocks as scoped block numbers alongside the
    /// scoped latest block. The MMR and the returned blocks won't contain the latest block.
    fn get_block_inputs_witnesses(
        &self,
        mut blocks: BTreeSet<BlockNumber>,
        account_ids: &[AccountId],
        nullifiers: &[Nullifier],
    ) -> Result<BlockInputWitnesses, GetBlockInputsError> {
        self.with_inner_read_blocking(|inner| {
            let latest_block_number = inner.latest_block_num();

            // The latest block is not yet in the chain MMR, so we can't (and don't need to) prove
            // its inclusion in the chain.
            blocks.remove(&latest_block_number);

            // Scoping the blocks doubles as the validation that none lies beyond the view's tip
            // (which equals the latest block number of this pinned snapshot). Scoped in descending
            // order, so the first failure carries the highest block number.
            let scoped_blocks = blocks
                .iter()
                .rev()
                .map(|&block| {
                    self.scope_block(block).ok_or(
                        GetBlockInputsError::UnknownBatchBlockReference {
                            highest_block_number: block,
                            latest_block_number,
                        },
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;

            // Fetch the partial MMR at the state of the latest block with authentication paths for
            // the provided set of blocks.
            //
            // SAFETY:
            // - The latest block num was retrieved from the inner blockchain from which we will
            //   also retrieve the proofs, so it is guaranteed to exist in that chain.
            // - Scoping above proved that no block in the set is greater than the latest block
            //   number *and* the latest block num was removed from the set. Therefore only block
            //   numbers smaller than latest block num remain in the set. Therefore all the block
            //   numbers are guaranteed to exist in the chain state at latest block num.
            let partial_mmr =
                inner.blockchain.partial_mmr_from_blocks(&blocks, latest_block_number).expect(
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

            Ok((
                self.tip(),
                scoped_blocks,
                account_witnesses,
                nullifier_witnesses,
                partial_mmr,
            ))
        })
    }
}
