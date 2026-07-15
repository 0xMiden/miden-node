//! Serialized block-write path for the store state.
//!
//! A single [`BlockWriter`] task owns the mutable trees and processes incoming [`WriteRequest`]s
//! one at a time via an mpsc channel. After each successful commit it publishes a new
//! [`InMemoryState`] snapshot via an [`ArcSwap`], making the updated trees immediately visible to
//! wait-free readers.

use std::sync::Arc;

use arc_swap::ArcSwap;
use miden_node_utils::ErrorReport;
use miden_node_utils::tracing::{miden_instrument, miden_span_record};
use miden_protocol::account::AccountUpdateDetails;
use miden_protocol::block::account_tree::AccountMutationSet;
use miden_protocol::block::nullifier_tree::{NullifierMutationSet, NullifierTree};
use miden_protocol::block::{BlockBody, BlockHeader, BlockNumber, Blockchain, SignedBlock};
use miden_protocol::crypto::merkle::smt::LargeSmt;
use miden_protocol::note::{NoteDetails, Nullifier};
use miden_protocol::transaction::OutputNote;
use miden_protocol::utils::serde::Serializable;
use tokio::sync::{mpsc, oneshot, watch};

use crate::account_state_forest::{AccountStateForest, AccountStateForestBackend};
use crate::accounts::AccountTreeWithHistory;
use crate::blocks::BlockStore;
use crate::db::{Db, NoteRecord};
use crate::errors::{ApplyBlockError, InvalidBlockError};
use crate::state::loader::TreeStorage;
use crate::state::{BlockCache, BlockNotification, InMemoryState, State};
use crate::{COMPONENT, HistoricalError, LOG_TARGET};

// WRITE REQUEST
// ================================================================================================

/// A request to apply a block, paired with a one-shot channel for the result.
pub(super) struct WriteRequest {
    signed_block: SignedBlock,
    result_tx: oneshot::Sender<Result<(), ApplyBlockError>>,
}

// WRITE HANDLE
// ================================================================================================

/// Handle for sending block-write requests to the [`BlockWriter`] task.
pub(super) struct WriteHandle {
    tx: mpsc::Sender<WriteRequest>,
}

impl WriteHandle {
    pub(super) fn new(tx: mpsc::Sender<WriteRequest>) -> Self {
        Self { tx }
    }

    /// Sends a block to the writer task and awaits its result.
    pub(super) async fn apply_block(
        &self,
        signed_block: SignedBlock,
    ) -> Result<(), ApplyBlockError> {
        let (result_tx, result_rx) = oneshot::channel();
        self.tx
            .send(WriteRequest { signed_block, result_tx })
            .await
            .map_err(|e| ApplyBlockError::WriterTaskSendFailed(e.as_report()))?;
        result_rx.await?
    }
}

impl State {
    /// Apply changes of a new block to the DB and in-memory data structures.
    ///
    /// Blocks are forwarded to the [`BlockWriter`] task, which processes them one at a time.
    /// Readers are unaffected while a block is being applied: they keep reading from the previous
    /// in-memory snapshot until the writer atomically publishes the new one.
    #[miden_instrument(
        target = COMPONENT,
        skip_all,
        err,
    )]
    pub async fn apply_block(&self, signed_block: SignedBlock) -> Result<(), ApplyBlockError> {
        self.write_handle.apply_block(signed_block).await
    }
}

// BLOCK WRITER
// ================================================================================================

/// Single-task owner of the mutable trees. Processes [`WriteRequest`]s serially.
///
/// The writer owns the writable trees directly, so no locks are held at any point: validation and
/// mutation-computation read the owned trees, the DB commit runs without touching them, and the
/// new [`InMemoryState`] snapshot is published atomically at the end.
pub(super) struct BlockWriter {
    pub db: Arc<Db>,
    pub block_store: Arc<BlockStore>,
    /// Atomically swappable pointer through which new snapshots are published.
    pub in_memory: Arc<ArcSwap<InMemoryState>>,
    pub committed_tip_tx: Arc<watch::Sender<BlockNumber>>,
    pub block_cache: BlockCache,
    pub rx: mpsc::Receiver<WriteRequest>,
    /// The mutable nullifier tree owned by this writer.
    pub nullifier_tree: NullifierTree<LargeSmt<TreeStorage>>,
    /// The mutable account tree owned by this writer.
    pub account_tree: AccountTreeWithHistory<TreeStorage>,
    /// The blockchain MMR owned by this writer.
    pub blockchain: Blockchain,
    /// The mutable account state forest owned by this writer.
    pub forest: AccountStateForest<AccountStateForestBackend>,
}

impl BlockWriter {
    /// Runs the writer loop, processing requests until all write handles are dropped.
    pub async fn run(mut self) {
        while let Some(req) = self.rx.recv().await {
            let result = self.write_block(req.signed_block).await;
            let _ = req.result_tx.send(result);
        }
    }

    /// Validates and commits a signed block to all persistent and in-memory stores.
    ///
    /// ## Note on state consistency
    ///
    /// Readers access the in-memory state through frozen snapshots, so consistency is maintained
    /// by ordering the commit steps rather than by locking:
    ///
    /// - the block is validated against the writer-owned trees and the DB prior to starting any
    ///   modifications.
    /// - the block is saved to the block store. Such blocks are considered candidates and are not
    ///   yet available for reading because the latest block pointer is not updated yet.
    /// - the DB transaction is committed. Concurrent readers still see the previous in-memory
    ///   snapshot; queries that combine DB and in-memory data are scoped by block number.
    /// - the in-memory structures owned by the writer are updated. On a crash in between, the trees
    ///   lag the DB by one block, which is detected by the consistency checks at startup (the same
    ///   crash semantics as the previous lock-based implementation).
    /// - the new snapshot is published atomically, making the block visible to readers.
    #[miden_instrument(
        target = COMPONENT,
        skip_all,
        err,
    )]
    async fn write_block(&mut self, signed_block: SignedBlock) -> Result<(), ApplyBlockError> {
        let header = signed_block.header();
        let body = signed_block.body();

        let block_num = header.block_num();
        let block_commitment = header.commitment();
        let num_transactions = body.transactions().as_slice().len();

        miden_span_record!(
            block.number = %block_num,
            block.commitment = %block_commitment,
            block.transactions.count = num_transactions,
        );

        self.validate_block_header(header, body).await?;

        // Compute the tree mutations and note records upfront, before any modifications.
        let (notes, nullifier_tree_update, account_tree_update) =
            tokio::task::block_in_place(|| {
                let notes = Self::build_note_records(header, body)?;
                let (nullifier_tree_update, account_tree_update) =
                    self.compute_tree_mutations(header, body)?;
                Ok::<_, ApplyBlockError>((notes, nullifier_tree_update, account_tree_update))
            })?;

        // Extract public account updates with patches before the block is moved into the DB
        // commit. Private accounts are filtered out since they don't expose their state changes.
        let account_patches =
            Vec::from_iter(body.updated_accounts().iter().filter_map(
                |update| match update.details() {
                    AccountUpdateDetails::Public(patch) => Some(patch.clone()),
                    AccountUpdateDetails::Private => None,
                },
            ));

        // Save the block to the block store. In a case of a failed DB transaction, the in-memory
        // state will be unchanged, but the file might still be written. Such blocks should be
        // considered candidates, not finalized blocks.
        let signed_block_bytes = signed_block.to_bytes();
        // Clone before moving into the block-save call so we can cache for replicas at commit.
        let cache_bytes = signed_block_bytes.clone();
        self.block_store.save_block(block_num, &signed_block_bytes).await?;

        // Commit to the DB. Readers continue to see the previous in-memory snapshot while the DB
        // commits; queries that combine DB and in-memory data are scoped by block number.
        self.db
            .apply_block(signed_block, notes)
            .await
            .map_err(|err| ApplyBlockError::DbUpdateTaskFailed(err.as_report()))?;

        // Apply the mutations to the writer-owned trees and build the new snapshot from reader
        // views of them. The reader views are point-in-time storage snapshots, so no tree data is
        // copied.
        let snapshot = tokio::task::block_in_place(|| {
            self.nullifier_tree
                .apply_mutations(nullifier_tree_update)
                .expect("nullifier tree mutation should succeed after validation");

            self.account_tree
                .apply_mutations(account_tree_update)
                .expect("account tree mutation should succeed after validation");

            self.blockchain.push(block_commitment);

            self.forest.apply_block_updates(block_num, account_patches);

            Arc::new(InMemoryState {
                nullifier_tree: self
                    .nullifier_tree
                    .reader()
                    .expect("nullifier tree snapshot creation should not fail"),
                account_tree: self.account_tree.reader(),
                blockchain: self.blockchain.clone(),
                forest: self.forest.reader().expect("forest snapshot creation should not fail"),
            })
        });

        // Atomically publish the new state. Readers that call `snapshot()` after this point will
        // see the updated state. Readers holding the old snapshot continue unaffected.
        self.in_memory.store(snapshot);

        // Push to cache and notify replica subscribers.
        self.block_cache
            .push(block_num, BlockNotification::new(block_num, cache_bytes))
            .expect("block cache receives sequential block numbers");
        let _ = self.committed_tip_tx.send(block_num);

        tracing::debug!(target: LOG_TARGET, "Block applied");

        Ok(())
    }

    /// Validates that the block header is consistent with the block body and the current state.
    #[miden_instrument(
        target = COMPONENT,
        skip_all,
        err,
    )]
    async fn validate_block_header(
        &self,
        header: &BlockHeader,
        body: &BlockBody,
    ) -> Result<(), ApplyBlockError> {
        // Validate that header and body match.
        let tx_commitment = body.transactions().commitment();
        if header.tx_commitment() != tx_commitment {
            return Err(InvalidBlockError::InvalidBlockTxCommitment {
                expected: tx_commitment,
                actual: header.tx_commitment(),
            }
            .into());
        }

        let block_num = header.block_num();

        // Validate that the applied block is the next block in sequence.
        let prev_block = self
            .db
            .select_block_header_by_block_num(None)
            .await?
            .ok_or(ApplyBlockError::DbBlockHeaderEmpty)?;
        let expected_block_num = prev_block.block_num().child();
        if block_num != expected_block_num {
            return Err(InvalidBlockError::NewBlockInvalidBlockNum {
                expected: expected_block_num,
                submitted: block_num,
            }
            .into());
        }
        if header.prev_block_commitment() != prev_block.commitment() {
            return Err(InvalidBlockError::NewBlockInvalidPrevCommitment.into());
        }

        Ok(())
    }

    /// Computes nullifier and account tree mutations, validating roots against the block header.
    #[miden_instrument(
        target = COMPONENT,
        skip_all,
        err,
    )]
    fn compute_tree_mutations(
        &self,
        header: &BlockHeader,
        body: &BlockBody,
    ) -> Result<(NullifierMutationSet, AccountMutationSet), ApplyBlockError> {
        let block_num = header.block_num();

        // nullifiers can be produced only once
        let duplicate_nullifiers: Vec<_> = body
            .created_nullifiers()
            .iter()
            .filter(|&nullifier| self.nullifier_tree.get_block_num(nullifier).is_some())
            .copied()
            .collect();
        if !duplicate_nullifiers.is_empty() {
            return Err(InvalidBlockError::DuplicatedNullifiers(duplicate_nullifiers).into());
        }

        // new_block.chain_root must be equal to the chain MMR root prior to the update
        let peaks = self.blockchain.peaks();
        if peaks.hash_peaks() != header.chain_commitment() {
            return Err(InvalidBlockError::NewBlockInvalidChainCommitment.into());
        }

        // compute update for nullifier tree
        let nullifier_tree_update = self
            .nullifier_tree
            .compute_mutations(
                body.created_nullifiers().iter().map(|nullifier| (*nullifier, block_num)),
            )
            .map_err(InvalidBlockError::NewBlockNullifierAlreadySpent)?;

        if nullifier_tree_update.as_mutation_set().root() != header.nullifier_root() {
            return Err(InvalidBlockError::NewBlockInvalidNullifierRoot.into());
        }

        // compute update for account tree
        let account_tree_update = self
            .account_tree
            .compute_mutations(
                body.updated_accounts()
                    .iter()
                    .map(|update| (update.account_id(), update.final_state_commitment())),
            )
            .map_err(|e| match e {
                HistoricalError::AccountTreeError(err) => {
                    InvalidBlockError::NewBlockDuplicateAccountIdPrefix(err)
                },
                HistoricalError::MerkleError(_) => {
                    panic!("Unexpected MerkleError during account tree mutation computation")
                },
            })?;

        if account_tree_update.as_mutation_set().root() != header.account_root() {
            return Err(InvalidBlockError::NewBlockInvalidAccountRoot.into());
        }

        Ok((nullifier_tree_update, account_tree_update))
    }

    /// Builds note records with inclusion proofs from the block body.
    #[miden_instrument(
        target = COMPONENT,
        skip_all,
        err,
    )]
    fn build_note_records(
        header: &BlockHeader,
        body: &BlockBody,
    ) -> Result<Vec<(NoteRecord, Option<Nullifier>)>, ApplyBlockError> {
        let block_num = header.block_num();

        let note_tree = body.compute_block_note_tree();
        if note_tree.root() != header.note_root() {
            return Err(InvalidBlockError::NewBlockInvalidNoteRoot.into());
        }

        let notes = body
            .output_notes()
            .map(|(note_index, note)| {
                let (details, attachments, nullifier) = match note {
                    OutputNote::Public(public) => (
                        Some(NoteDetails::from(public.as_note())),
                        public.as_note().attachments().clone(),
                        Some(public.as_note().nullifier()),
                    ),
                    OutputNote::Private(private) => (None, private.attachments().clone(), None),
                };

                let inclusion_path = note_tree.open(note_index);

                let note_record = NoteRecord {
                    block_num,
                    note_index,
                    note_id: note.id().as_word(),
                    metadata: *note.metadata(),
                    details,
                    attachments,
                    inclusion_path,
                };

                Ok((note_record, nullifier))
            })
            .collect::<Result<Vec<_>, InvalidBlockError>>()?;

        Ok(notes)
    }
}
