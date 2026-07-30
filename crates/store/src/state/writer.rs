//! Serialized block-write path for the store state.
//!
//! A single [`WriteWorker`] task owns the mutable trees and processes incoming [`WriteRequest`]s
//! one at a time via an mpsc channel. After each successful commit it publishes a new
//! [`InMemoryState`] snapshot via an [`ArcSwap`], making the updated trees immediately visible to
//! wait-free readers.

use std::fmt::Display;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use arc_swap::ArcSwap;
use miden_node_utils::ErrorReport;
use miden_node_utils::shutdown::CancellationToken;
use miden_node_utils::tracing::{miden_instrument, miden_span_record};
use miden_protocol::Word;
use miden_protocol::account::AccountUpdateDetails;
use miden_protocol::block::account_tree::AccountMutationSet;
use miden_protocol::block::nullifier_tree::{NullifierMutationSet, NullifierTree};
use miden_protocol::block::{BlockBody, BlockHeader, BlockNumber, Blockchain, SignedBlock};
use miden_protocol::crypto::merkle::smt::LargeSmt;
use miden_protocol::note::{NoteDetails, Nullifier};
use miden_protocol::transaction::OutputNote;
use miden_protocol::utils::serde::Serializable;
use tokio::sync::{mpsc, oneshot, watch};

use crate::account_state_forest::{
    AccountStateForest,
    AccountStateForestBackend,
    PreparedAccountStateForestBlockUpdate,
};
use crate::accounts::AccountTreeWithHistory;
use crate::blocks::BlockStore;
use crate::db::{Db, NoteRecord};
use crate::errors::{ApplyBlockError, InvalidBlockError};
use crate::state::block_lifecycle::{BlockLifecycle, lifecycle_events_enabled};
use crate::state::loader::TreeStorage;
use crate::state::{
    BlockCache,
    BlockNotification,
    BlockWriter,
    InMemoryState,
    SNAPSHOTS_LIVE_WARN_THRESHOLD,
    SnapshotGuard,
};
use crate::{COMPONENT, HistoricalError, LOG_TARGET};

// WRITE REQUEST
// ================================================================================================

/// A request to apply a block, paired with a one-shot channel for the result.
pub(super) struct WriteRequest {
    signed_block: SignedBlock,
    result_tx: oneshot::Sender<Result<(), ApplyBlockError>>,
}

impl BlockWriter {
    /// Apply changes of a new block to the DB and in-memory data structures.
    ///
    /// Blocks are forwarded to the [`WriteWorker`] task, which processes them one at a time.
    /// Readers are unaffected while a block is being applied: they keep reading from the previous
    /// in-memory snapshot until the writer atomically publishes the new one.
    #[miden_instrument(
        target = COMPONENT,
        skip_all,
        err,
    )]
    pub async fn apply_block(&self, signed_block: SignedBlock) -> Result<(), ApplyBlockError> {
        let (result_tx, result_rx) = oneshot::channel();
        self.write_tx
            .send(WriteRequest { signed_block, result_tx })
            .await
            .map_err(|e| ApplyBlockError::WriterTaskSendFailed(e.as_report()))?;
        result_rx.await?
    }
}

// WRITE WORKER
// ================================================================================================

/// Single-task owner of the mutable trees. Processes [`WriteRequest`]s serially.
///
/// The writer owns the writable trees directly, so no locks are held at any point: validation and
/// mutation-computation read the owned trees, the DB commit runs without touching them, and the
/// new [`InMemoryState`] snapshot is published atomically at the end.
pub(super) struct WriteWorker {
    pub db: Arc<Db>,
    pub block_store: Arc<BlockStore>,
    /// Atomically swappable pointer through which new snapshots are published.
    pub in_memory: Arc<ArcSwap<InMemoryState>>,
    pub committed_tip_tx: Arc<watch::Sender<BlockNumber>>,
    pub block_cache: BlockCache,
    pub rx: mpsc::Receiver<WriteRequest>,
    /// Token signalling node shutdown; the writer stops accepting new requests once cancelled.
    pub shutdown: CancellationToken,
    /// The mutable nullifier tree owned by this writer.
    pub nullifier_tree: NullifierTree<LargeSmt<TreeStorage>>,
    /// The mutable account tree owned by this writer.
    pub account_tree: AccountTreeWithHistory<TreeStorage>,
    /// The blockchain MMR owned by this writer.
    pub blockchain: Blockchain,
    /// The mutable account state forest owned by this writer.
    pub forest: AccountStateForest<AccountStateForestBackend>,
    /// Shared counter of live snapshot generations, for observability.
    pub snapshots_live: Arc<AtomicUsize>,
}

/// Note records and state mutations computed from a validated block, before any modifications.
struct PreparedBlockUpdate {
    notes: Vec<(NoteRecord, Option<Nullifier>)>,
    nullifier_tree_update: NullifierMutationSet,
    account_tree_update: AccountMutationSet,
    account_forest_update: PreparedAccountStateForestBlockUpdate<AccountStateForestBackend>,
}

impl WriteWorker {
    /// Runs the writer loop, processing requests until shutdown is signalled or the
    /// [`BlockWriter`] (holding the only request sender) is dropped.
    ///
    /// Cancellation is only observed between requests: an in-flight block write always runs to
    /// completion, so shutdown never leaves the trees lagging the committed database state.
    /// Requests still queued when cancellation fires are dropped, failing their senders.
    pub async fn run(mut self) {
        loop {
            let req = tokio::select! {
                biased;
                () = self.shutdown.cancelled() => break,
                req = self.rx.recv() => match req {
                    Some(req) => req,
                    None => break,
                },
            };
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

        let block_lifecycle =
            lifecycle_events_enabled().then(|| BlockLifecycle::from_block_body(block_num, body));
        let unresolved_note_nullifiers = block_lifecycle
            .as_ref()
            .map_or_else(Vec::new, BlockLifecycle::unresolved_note_nullifiers);

        // Compute the tree and forest mutations and note records upfront, before any modifications.
        // The writer is the sole forest mutator, so the precomputed forest update stays valid until
        // it is applied after the DB commit below.
        let PreparedBlockUpdate {
            notes,
            nullifier_tree_update,
            account_tree_update,
            account_forest_update,
        } = tokio::task::block_in_place(|| self.prepare_block_update(header, body))?;
        let precomputed_public_states = account_forest_update.account_states.clone();

        // Save the block to the block store. In a case of a failed DB transaction, the in-memory
        // state will be unchanged, but the file might still be written. Such blocks should be
        // considered candidates, not finalized blocks.
        let signed_block_bytes = signed_block.to_bytes();
        // Clone before moving into the block-save call so we can cache for replicas at commit.
        let cache_bytes = signed_block_bytes.clone();
        self.block_store.save_block(block_num, &signed_block_bytes).await?;

        // Commit to the DB. Readers continue to see the previous in-memory snapshot while the DB
        // commits; queries that combine DB and in-memory data are scoped by block number.
        let resolved_note_ids = self
            .db
            .apply_block(signed_block, notes, precomputed_public_states, unresolved_note_nullifiers)
            .await
            .map_err(|err| ApplyBlockError::DbUpdateTaskFailed(err.as_report()))?;

        // The DB is committed at this point, so the prepared mutations must be applied and any
        // failure to do so aborts the process.
        let snapshot = tokio::task::block_in_place(|| {
            self.apply_prepared_mutations(
                block_num,
                block_commitment,
                nullifier_tree_update,
                account_tree_update,
                account_forest_update,
            )
        });

        // Atomically publish the new state. Readers that call `snapshot()` after this point will
        // see the updated state. Readers holding the old snapshot continue unaffected.
        self.in_memory.store(snapshot);

        let snapshots_live = self.check_live_snapshots(block_num);
        miden_span_record!(snapshots.live = snapshots_live);

        // Push to cache and notify replica subscribers.
        self.block_cache
            .push(block_num, BlockNotification::new(block_num, cache_bytes))
            .expect("block cache receives sequential block numbers");
        let _ = self.committed_tip_tx.send(block_num);

        if let Some(block_lifecycle) = block_lifecycle {
            block_lifecycle.emit(&resolved_note_ids);
        }
        tracing::debug!(target: LOG_TARGET, "Block applied");

        Ok(())
    }

    /// Returns the number of live snapshot generations, warning when slow readers are pinning too
    /// many old generations in memory.
    ///
    /// The count is returned rather than recorded here because `miden_span_record!` must be used
    /// within a `#[miden_instrument]` function.
    fn check_live_snapshots(&self, block_num: BlockNumber) -> u64 {
        let snapshots_live = self.snapshots_live.load(Ordering::Relaxed) as u64;
        if snapshots_live > SNAPSHOTS_LIVE_WARN_THRESHOLD {
            tracing::warn!(
                target: COMPONENT,
                block_num = block_num.as_u32(),
                snapshots.live = snapshots_live,
                "too many live state snapshots; slow readers are pinning old generations",
            );
        }
        snapshots_live
    }

    /// Computes the note records and all tree and forest mutations for a block, without mutating
    /// any state.
    ///
    /// May block on backend I/O, so it must run on Tokio's blocking path. The returned forest
    /// update is bound to the forest state observed here; it remains valid until applied because
    /// the writer is the sole forest mutator.
    fn prepare_block_update(
        &self,
        header: &BlockHeader,
        body: &BlockBody,
    ) -> Result<PreparedBlockUpdate, ApplyBlockError> {
        let notes = Self::build_note_records(header, body)?;
        let (nullifier_tree_update, account_tree_update) =
            self.compute_tree_mutations(header, body)?;

        // Public account updates carry patches; private accounts are filtered out since they don't
        // expose their state changes.
        let account_patches =
            body.updated_accounts().iter().filter_map(|update| match update.details() {
                AccountUpdateDetails::Public(patch) => Some(patch.clone()),
                AccountUpdateDetails::Private => None,
            });
        let account_forest_update = self
            .forest
            .compute_block_update_mutations(header.block_num(), account_patches)
            .map_err(ApplyBlockError::AccountStateForestPreparation)?;

        Ok(PreparedBlockUpdate {
            notes,
            nullifier_tree_update,
            account_tree_update,
            account_forest_update,
        })
    }

    /// Applies the prepared mutations to the writer-owned trees and builds the new snapshot from
    /// reader views of them. The reader views are point-in-time storage snapshots, so no tree data
    /// is copied.
    ///
    /// Must only be called after the corresponding DB commit: at that point the mutations are part
    /// of canonical state, so a failure to apply them leaves the trees divergent and aborts the
    /// process. May block on backend I/O, so it must run on Tokio's blocking path.
    fn apply_prepared_mutations(
        &mut self,
        block_num: BlockNumber,
        block_commitment: Word,
        nullifier_tree_update: NullifierMutationSet,
        account_tree_update: AccountMutationSet,
        account_forest_update: PreparedAccountStateForestBlockUpdate<AccountStateForestBackend>,
    ) -> Arc<InMemoryState> {
        self.nullifier_tree
            .apply_mutations(nullifier_tree_update)
            .unwrap_or_else(|error| {
                Self::abort_after_post_commit_failure("nullifier tree", &error)
            });

        self.account_tree
            .apply_mutations(account_tree_update)
            .unwrap_or_else(|error| Self::abort_after_post_commit_failure("account tree", &error));

        self.blockchain.push(block_commitment);

        self.forest
            .apply_precomputed_block_update(block_num, account_forest_update)
            .unwrap_or_else(|error| {
                Self::abort_after_post_commit_failure("account-state forest", &error)
            });

        Arc::new(InMemoryState {
            nullifier_tree: self
                .nullifier_tree
                .reader()
                .expect("nullifier tree snapshot creation should not fail"),
            account_tree: self.account_tree.reader(),
            blockchain: self.blockchain.clone(),
            forest: self.forest.reader().expect("forest snapshot creation should not fail"),
            _guard: SnapshotGuard::new(Arc::clone(&self.snapshots_live), block_num),
        })
    }

    /// Terminates after a persistent state failure that occurred after the canonical DB commit.
    ///
    /// Returning would expose components at different block heights. Tests panic so the fatal path
    /// can be asserted without terminating the test process; production aborts immediately.
    fn abort_after_post_commit_failure(component: &str, error: &impl Display) -> ! {
        tracing::error!(
            target: LOG_TARGET,
            component,
            error = %error,
            "persistent state update failed after database commit; aborting"
        );

        #[cfg(test)]
        panic!("{component} update failed after database commit: {error}");

        #[cfg(not(test))]
        std::process::abort();
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

        // A nullifier can only ever be created once, so the block is invalid if any of its
        // nullifiers are already recorded in the tree.
        let duplicate_nullifiers: Vec<_> = body
            .created_nullifiers()
            .iter()
            .filter(|&nullifier| self.nullifier_tree.get_block_num(nullifier).is_some())
            .copied()
            .collect();
        if !duplicate_nullifiers.is_empty() {
            return Err(InvalidBlockError::DuplicatedNullifiers(duplicate_nullifiers).into());
        }

        // The header's chain commitment must equal the chain MMR root prior to this block.
        let peaks = self.blockchain.peaks();
        if peaks.hash_peaks() != header.chain_commitment() {
            return Err(InvalidBlockError::NewBlockInvalidChainCommitment.into());
        }

        // Compute the nullifier tree mutations and verify that they produce the nullifier root
        // claimed in the header.
        let nullifier_tree_update = self
            .nullifier_tree
            .compute_mutations(
                body.created_nullifiers().iter().map(|nullifier| (*nullifier, block_num)),
            )
            .map_err(InvalidBlockError::NewBlockNullifierAlreadySpent)?;

        if nullifier_tree_update.as_mutation_set().root() != header.nullifier_root() {
            return Err(InvalidBlockError::NewBlockInvalidNullifierRoot.into());
        }

        // Compute the account tree mutations and verify that they produce the account root claimed
        // in the header.
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
