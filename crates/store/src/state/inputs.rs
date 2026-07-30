//! Block-producer input queries: batch, block, and transaction inputs.
//!
//! These read paths combine in-memory snapshot data (tree witnesses, partial MMRs) with database
//! lookups, scoping the latter by the snapshot's tip so both sources describe the same block
//! height.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ops::ControlFlow;

use miden_node_proto::domain::batch::BatchInputs;
use miden_node_utils::formatting::format_array;
use miden_node_utils::tracing::miden_instrument;
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::block::account_tree::AccountWitness;
use miden_protocol::block::nullifier_tree::NullifierWitness;
use miden_protocol::block::{BlockInputs, BlockNumber};
use miden_protocol::crypto::merkle::mmr::PartialMmr;
use miden_protocol::note::Nullifier;
use miden_protocol::transaction::PartialBlockchain;

use crate::COMPONENT;
use crate::db::NullifierInfo;
use crate::errors::{DatabaseError, GetBatchInputsError, GetBlockInputsError};
use crate::state::StateView;

// STRUCTURES
// ================================================================================================

#[derive(Debug, Default)]
pub struct TransactionInputs {
    pub account_commitment: Word,
    pub nullifiers: Vec<NullifierInfo>,
    pub found_unauthenticated_notes: HashSet<Word>,
    pub new_account_id_prefix_is_unique: Option<bool>,
}

type BlockInputWitnesses = (
    BlockNumber,
    BTreeMap<AccountId, AccountWitness>,
    BTreeMap<Nullifier, NullifierWitness>,
    PartialMmr,
);

// INPUT QUERIES
// ================================================================================================

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

        let latest_block_num = self.tip();

        let highest_block_num =
            *blocks.last().expect("we should have checked for empty block references");
        if highest_block_num > latest_block_num {
            return Err(GetBatchInputsError::UnknownTransactionBlockReference {
                highest_block_num,
                latest_block_num,
            });
        }

        // Remove the latest block from the to-be-tracked blocks as it will be the reference
        // block for the batch itself and thus added to the MMR within the batch kernel, so
        // there is no need to prove its inclusion.
        blocks.remove(&latest_block_num);

        // SAFETY:
        // - The latest block num was retrieved from the view's blockchain from which we will
        //   also retrieve the proofs, so it is guaranteed to exist in that chain.
        // - We have checked that no block number in the blocks set is greater than latest block
        //   number *and* latest block num was removed from the set. Therefore only block
        //   numbers smaller than latest block num remain in the set. Therefore all the block
        //   numbers are guaranteed to exist in the chain state at latest block num.
        let partial_mmr =
            self.blockchain().partial_mmr_from_blocks(&blocks, latest_block_num).expect(
                "latest block num should exist and all blocks in set should be < than latest block",
            );

        let batch_reference_block = latest_block_num;

        // Fetch the reference block of the batch as part of this query, so we can avoid looking it
        // up in a separate DB access.
        let mut headers = self
            .db()
            .select_block_headers(blocks.into_iter().chain(std::iter::once(batch_reference_block)))
            .await
            .map_err(GetBatchInputsError::SelectBlockHeaderError)?;

        // Find and remove the batch reference block as we don't want to add it to the chain MMR.
        let header_index = headers
            .iter()
            .enumerate()
            .find_map(|(index, header)| {
                (header.block_num() == batch_reference_block).then_some(index)
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

    /// Returns data needed by the block producer to construct and prove the next block.
    pub async fn get_block_inputs(
        &self,
        account_ids: Vec<AccountId>,
        nullifiers: Vec<Nullifier>,
        unauthenticated_note_commitments: BTreeSet<Word>,
        reference_blocks: BTreeSet<BlockNumber>,
    ) -> Result<BlockInputs, GetBlockInputsError> {
        // Get the note inclusion proofs from the DB. We do this first so we have to acquire the
        // lock to the state just once. There we need the reference blocks of the note proofs to get
        // their authentication paths in the chain MMR.
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

        // Fetch the block headers for all blocks in the partial MMR plus the latest one which will
        // be used as the previous block header of the block being built.
        let mut headers = self
            .db()
            .select_block_headers(blocks.into_iter().chain(std::iter::once(latest_block_number)))
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

    /// Returns data needed by the block producer to verify transactions validity.
    #[miden_instrument(
        target = COMPONENT,
        skip_all,
        fields(
            account.id=%account_id,
            nullifiers = %format_array(nullifiers),
        ),
    )]
    pub async fn get_transaction_inputs(
        &self,
        account_id: AccountId,
        nullifiers: &[Nullifier],
        unauthenticated_note_commitments: Vec<Word>,
    ) -> Result<TransactionInputs, DatabaseError> {
        let tree_inputs = self.with_inner_read_blocking(|inner| {
            let account_commitment = inner.account_tree.get_latest_commitment(account_id);

            let new_account_id_prefix_is_unique = if account_commitment.is_empty() {
                Some(!inner.account_tree.contains_account_id_prefix_in_latest(account_id.prefix()))
            } else {
                None
            };

            // Non-unique account Id prefixes for new accounts are not allowed, so the transaction
            // cannot be valid and the response is already complete.
            if let Some(false) = new_account_id_prefix_is_unique {
                return ControlFlow::Break(TransactionInputs {
                    new_account_id_prefix_is_unique,
                    ..Default::default()
                });
            }

            let nullifiers = nullifiers
                .iter()
                .map(|nullifier| NullifierInfo {
                    nullifier: *nullifier,
                    block_num: inner.nullifier_tree.get_block_num(nullifier).unwrap_or_default(),
                })
                .collect();

            ControlFlow::Continue((account_commitment, nullifiers, new_account_id_prefix_is_unique))
        });
        // `Break` carries a complete response (duplicate account ID prefix), so it is returned
        // as-is without the note lookup below; `Continue` carries the tree reads needed to build
        // the full response.
        let (account_commitment, nullifiers, new_account_id_prefix_is_unique) = match tree_inputs {
            ControlFlow::Continue(inputs) => inputs,
            ControlFlow::Break(response) => return Ok(response),
        };

        // Scope the note lookup by the view's tip so the result is consistent with the tree reads
        // above: mid-apply, the DB may already contain notes from a block the snapshot does not
        // include yet.
        let found_unauthenticated_notes = self
            .db()
            .select_existing_note_commitments(unauthenticated_note_commitments, self.tip())
            .await?;

        Ok(TransactionInputs {
            account_commitment,
            nullifiers,
            found_unauthenticated_notes,
            new_account_id_prefix_is_unique,
        })
    }
}
