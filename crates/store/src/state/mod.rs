//! Abstraction to synchronize state modifications.
//!
//! The [State] provides data access and modifications methods, its main purpose is to ensure that
//! data is atomically written, and that reads are consistent.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::num::NonZeroUsize;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use miden_node_proto::domain::batch::BatchInputs;
use miden_node_utils::ErrorReport;
use miden_node_utils::clap::StorageOptions;
use miden_node_utils::formatting::format_array;
use miden_node_utils::shutdown::CancellationToken;
use miden_node_utils::tracing::miden_instrument;
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::block::account_tree::AccountWitness;
use miden_protocol::block::nullifier_tree::{NullifierTree, NullifierWitness};
use miden_protocol::block::{BlockHeader, BlockInputs, BlockNumber, Blockchain};
use miden_protocol::crypto::merkle::mmr::{MmrProof, PartialMmr};
use miden_protocol::crypto::merkle::smt::LargeSmt;
use miden_protocol::note::{NoteId, NoteScript, Nullifier};
use miden_protocol::transaction::PartialBlockchain;
use tokio::sync::watch;
use tracing::Span;

use crate::account_state_forest::{
    AccountStateForest,
    AccountStateForestBackend,
    AccountStateForestBackendReader,
};
use crate::accounts::AccountTreeWithHistory;
use crate::blocks::BlockStore;
use crate::db::{Db, NoteRecord, NullifierInfo};
use crate::errors::{
    DatabaseError,
    GetBatchInputsError,
    GetBlockHeaderError,
    GetBlockInputsError,
    StateInitializationError,
};
use crate::proven_tip::ProvenTipWriter;
use crate::{COMPONENT, DataDirectory, DatabaseOptions};

/// Number of recent committed blocks held in the in-memory cache for replica subscriptions.
const BLOCK_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(512).unwrap();

/// Number of recent block proofs held in the in-memory cache for replica subscriptions.
const PROOF_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(512).unwrap();

/// Snapshot lifetime above which [`SnapshotGuard`] logs a warning on release.
///
/// Readers are expected to be request-scoped, so a snapshot outliving several block intervals
/// indicates a slow or leaked reader pinning a `RocksDB` snapshot (see [`SnapshotGuard`]).
const SNAPSHOT_LIFETIME_WARN_THRESHOLD: Duration = Duration::from_secs(10);

/// Number of live snapshot generations above which the [`BlockWriter`] logs a warning after
/// publishing a new snapshot.
///
/// Steady state is 1-2 generations: the just-published snapshot plus predecessors briefly pinned
/// by in-flight requests. A sustained higher count means slow or leaked readers are holding old
/// generations alive (see [`SnapshotGuard`]).
const SNAPSHOTS_LIVE_WARN_THRESHOLD: u64 = 4;

mod loader;
use loader::{
    ACCOUNT_STATE_FOREST_STORAGE_DIR,
    ACCOUNT_TREE_STORAGE_DIR,
    AccountForestLoader,
    NULLIFIER_TREE_STORAGE_DIR,
    TreeStorage,
    TreeStorageLoader,
    TreeStorageReader,
    load_mmr,
    verify_account_state_forest_consistency,
    verify_tree_consistency,
};

mod replica;
pub use replica::{BlockCache, BlockNotification, ProofCache, ProofNotification};

mod account;

mod apply_block;
mod apply_proof;
mod bootstrap;
mod disk_monitor;
mod sync_state;
mod writer;
use writer::{BlockWriter, WriteHandle};

// FINALITY
// ================================================================================================

/// The finality level for chain tip queries.
#[derive(Debug, Clone, Copy)]
pub enum Finality {
    /// The latest committed (but not necessarily proven) block.
    Committed,
    /// The latest block that has been proven in an unbroken sequence from genesis.
    Proven,
}

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

/// RAII member of [`InMemoryState`] that tracks the number of live snapshot generations.
///
/// [`InMemoryState`] is dropped exactly when the last [`Arc`] reference to it is released, so the
/// shared counter reports how many distinct snapshot generations are currently pinned by readers.
/// A sustained count above 1-2 means slow readers are holding old generations alive. Each
/// generation pins a `RocksDB` snapshot, which delays garbage collection of superseded key
/// versions during compaction (compaction itself keeps running); the retained garbage grows with
/// write churn for as long as the snapshot is held and is reclaimed once it is released.
///
/// Readers are expected to be request-scoped, so snapshot lifetimes should be well under a block
/// interval. A lifetime exceeding [`SNAPSHOT_LIFETIME_WARN_THRESHOLD`] is logged at warn level.
pub(crate) struct SnapshotGuard {
    live: Arc<AtomicUsize>,
    created_at: Instant,
    block_num: BlockNumber,
}

impl SnapshotGuard {
    pub(super) fn new(live: Arc<AtomicUsize>, block_num: BlockNumber) -> Self {
        live.fetch_add(1, Ordering::Relaxed);
        Self {
            live,
            created_at: Instant::now(),
            block_num,
        }
    }
}

impl Drop for SnapshotGuard {
    fn drop(&mut self) {
        let remaining = self.live.fetch_sub(1, Ordering::Relaxed) - 1;
        let lifetime = self.created_at.elapsed();
        let lifetime_ms = u64::try_from(lifetime.as_millis()).unwrap_or(u64::MAX);
        let block_num = self.block_num.as_u32();
        if lifetime > SNAPSHOT_LIFETIME_WARN_THRESHOLD {
            tracing::warn!(
                target: COMPONENT,
                block_num,
                snapshot.lifetime_ms = lifetime_ms,
                snapshots.live = remaining,
                "state snapshot held for excessive time",
            );
        } else {
            tracing::debug!(
                target: COMPONENT,
                block_num,
                snapshot.lifetime_ms = lifetime_ms,
                snapshots.live = remaining,
                "state snapshot released",
            );
        }
    }
}

/// Immutable snapshot of the in-memory tree state published after each committed block.
///
/// The trees are backed by read-only snapshot storage ([`TreeStorageReader`] /
/// [`AccountStateForestBackendReader`]), so any number of readers can access the data concurrently
/// without holding a lock and without blocking the writer.
pub(crate) struct InMemoryState {
    nullifier_tree: NullifierTree<LargeSmt<TreeStorageReader>>,
    blockchain: Blockchain,
    account_tree: AccountTreeWithHistory<TreeStorageReader>,
    forest: AccountStateForest<AccountStateForestBackendReader>,
    /// Keeps the live-snapshot count accurate; see [`SnapshotGuard`].
    _guard: SnapshotGuard,
}

impl InMemoryState {
    /// Returns the latest block number.
    fn latest_block_num(&self) -> BlockNumber {
        self.blockchain
            .chain_tip()
            .expect("chain should always have at least the genesis block")
    }
}

// CHAIN STATE
// ================================================================================================

/// The rollup state.
pub struct State {
    /// Root directory containing the store's on-disk data.
    data_directory: PathBuf,

    /// The database which stores block headers, nullifiers, notes, and the latest states of
    /// accounts.
    db: Arc<Db>,

    /// The block store which stores full block contents for all blocks.
    block_store: Arc<BlockStore>,

    /// Atomically swappable pointer to the latest in-memory state snapshot.
    ///
    /// Readers load the snapshot wait-free via [`ArcSwap::load_full`]; the [`BlockWriter`] task
    /// atomically replaces the pointer after each committed block. Readers holding an old snapshot
    /// are unaffected by the swap.
    in_memory: Arc<ArcSwap<InMemoryState>>,

    /// Handle for sending block-write requests to the [`BlockWriter`] task.
    write_handle: WriteHandle,

    /// The latest proven-in-sequence block number, updated by the proof scheduler or `apply_proof`.
    proven_tip: ProvenTipWriter,

    /// Watch sender fired after each block is committed. Replicas subscribe via
    /// `subscribe_committed_tip()` to be woken when new blocks arrive.
    committed_tip_tx: Arc<watch::Sender<BlockNumber>>,

    /// FIFO cache of recent committed blocks for replica subscriptions. When a subscriber needs a
    /// block that has been evicted, it falls back to loading from the block store.
    pub(crate) block_cache: BlockCache,

    /// FIFO cache of recent block proofs for replica subscriptions. When a subscriber needs a proof
    /// that has been evicted, it falls back to loading from the block store.
    pub(crate) proof_cache: ProofCache,
}

/// A loaded store state whose block writer has not been started yet.
///
/// Returned by [`State::load`]. [`Self::start`] spawns the writer and yields the usable
/// [`State`]; since this is the only way to obtain one, [`State::apply_block`] can always make
/// progress.
#[must_use = "call `start` to spawn the block writer and obtain the state"]
pub struct LoadedState {
    state: State,
    writer: BlockWriter,
}

impl LoadedState {
    /// Spawns the block writer onto the current runtime and returns the state together with the
    /// writer task's join handle.
    ///
    /// The writer exits once the shutdown token passed to [`State::load`] is cancelled or the last
    /// state reference (holding the only write handle) is dropped — an in-flight block write
    /// always completes first. Awaiting the returned handle after either event guarantees the
    /// writer has released the tree storage it owns; a join error carries a writer panic.
    ///
    /// Callers without a token to cancel should stop the store via [`State::stop`] rather than
    /// dropping and joining by hand.
    pub fn start(self) -> (Arc<State>, tokio::task::JoinHandle<()>) {
        let writer_task = tokio::spawn(self.writer.run());
        (Arc::new(self.state), writer_task)
    }
}

impl State {
    /// Stops the store, waiting until the block writer has released the tree storage it owns.
    ///
    /// Consumes the last state reference — closing the write channel the writer listens on — and
    /// then joins the writer task returned by [`LoadedState::start`]. The drop must precede the
    /// join or the writer never observes the closed channel; doing both here keeps that ordering
    /// out of caller hands.
    ///
    /// Callers that need the storage released deterministically must use this method instead of
    /// dropping: the node's `recover` command stops the store before the process exits, and the
    /// stress-test's store seeding stops it so the same data directory can be re-loaded (or its
    /// temporary directory deleted) immediately afterwards. The running node does not use this
    /// method — its writer exits via the shutdown token passed to [`State::load`] and is joined
    /// through the node's task set.
    ///
    /// # Errors
    ///
    /// Returns both pieces unchanged if other references to the state are still alive.
    ///
    /// # Panics
    ///
    /// Panics if the writer task panicked.
    pub async fn stop(
        self: Arc<Self>,
        writer_task: tokio::task::JoinHandle<()>,
    ) -> Result<(), (Arc<Self>, tokio::task::JoinHandle<()>)> {
        match Arc::try_unwrap(self) {
            Ok(state) => {
                drop(state);
                writer_task.await.expect("block writer task should not panic");
                Ok(())
            },
            Err(state) => Err((state, writer_task)),
        }
    }
}

impl State {
    // CONSTRUCTOR
    // --------------------------------------------------------------------------------------------

    /// Loads the state from the data directory.
    ///
    /// The loaded state owns all store data structures and exposes subscription methods for
    /// sequencer and replica tasks. Call [`LoadedState::start`] on the result to spawn the block
    /// writer and obtain the usable [`State`].
    #[miden_instrument(
        target = COMPONENT,
        skip_all,
    )]
    pub async fn load(
        data_path: &Path,
        storage_options: StorageOptions,
        shutdown: CancellationToken,
    ) -> Result<LoadedState, StateInitializationError> {
        Self::load_with_database_options(
            data_path,
            storage_options,
            DatabaseOptions::default(),
            shutdown,
        )
        .await
    }

    /// Loads the state from the data directory using explicit database options.
    ///
    /// The loaded state owns all store data structures and exposes subscription methods for
    /// sequencer and replica tasks. Call [`LoadedState::start`] on the result to spawn the block
    /// writer and obtain the usable [`State`].
    #[miden_instrument(
        target = COMPONENT,
        skip_all,
    )]
    pub async fn load_with_database_options(
        data_path: &Path,
        storage_options: StorageOptions,
        database_options: DatabaseOptions,
        shutdown: CancellationToken,
    ) -> Result<LoadedState, StateInitializationError> {
        let data_directory = DataDirectory::load(data_path.to_path_buf())
            .map_err(StateInitializationError::DataDirectoryLoadError)?;

        let block_store = Arc::new(
            BlockStore::load(data_directory.block_store_dir())
                .map_err(StateInitializationError::BlockStoreLoadError)?,
        );

        let database_filepath = data_directory.database_path();
        let mut db = Db::load_with_pool_size(
            database_filepath.clone(),
            database_options.connection_pool_size,
        )
        .await
        .map_err(StateInitializationError::DatabaseLoadError)?;

        let blockchain = load_mmr(&mut db).await?;
        let latest_block_num = blockchain.chain_tip().unwrap_or(BlockNumber::GENESIS);

        #[cfg(feature = "rocksdb")]
        let (account_storage_config, nullifier_storage_config, forest_storage_config) = (
            storage_options.account_tree.into(),
            storage_options.nullifier_tree.into(),
            storage_options.account_state_forest.into(),
        );
        #[cfg(not(feature = "rocksdb"))]
        let (account_storage_config, nullifier_storage_config, forest_storage_config) = {
            let _ = &storage_options;
            ((), (), ())
        };
        let account_storage =
            TreeStorage::create(data_path, &account_storage_config, ACCOUNT_TREE_STORAGE_DIR)?;
        let account_tree = account_storage.load_account_tree(&mut db).await?;

        let nullifier_storage =
            TreeStorage::create(data_path, &nullifier_storage_config, NULLIFIER_TREE_STORAGE_DIR)?;
        let nullifier_tree = nullifier_storage.load_nullifier_tree(&mut db).await?;

        // Verify that tree roots match the expected roots from the database. This catches any
        // divergence between persistent storage and the database caused by corruption or incomplete
        // shutdown.
        verify_tree_consistency(account_tree.root(), nullifier_tree.root(), &mut db).await?;

        let account_tree = AccountTreeWithHistory::new(account_tree, latest_block_num);

        let forest_backend = AccountStateForestBackend::create(
            data_path,
            &forest_storage_config,
            ACCOUNT_STATE_FOREST_STORAGE_DIR,
        )?;
        let forest = forest_backend.load_account_state_forest(&mut db, latest_block_num).await?;
        verify_account_state_forest_consistency(&forest, &mut db).await?;

        let db = Arc::new(db);

        // Initialize the proven tip from the block store.
        let proven_tip_init = block_store
            .load_proven_tip()
            .map_err(StateInitializationError::ProvenTipLoadError)?;
        let (proven_tip, _rx) = ProvenTipWriter::new(proven_tip_init);

        // Committed-tip watch: fires after each successful apply_block.
        let (committed_tip_tx, _rx) = watch::channel(latest_block_num);
        let committed_tip_tx = Arc::new(committed_tip_tx);

        let block_cache = BlockCache::new(BLOCK_CACHE_CAPACITY);
        let proof_cache = ProofCache::new(PROOF_CACHE_CAPACITY);

        // Shared counter of live snapshot generations, for observability.
        let snapshots_live = Arc::new(AtomicUsize::new(0));

        // Create the initial snapshot from reader views of the just-loaded trees.
        let initial_snapshot = Arc::new(InMemoryState {
            nullifier_tree: nullifier_tree
                .reader()
                .map_err(|e| StateInitializationError::NullifierTreeIoError(e.as_report()))?,
            account_tree: account_tree.reader(),
            forest: forest
                .reader()
                .map_err(|e| StateInitializationError::AccountStateForestIoError(e.as_report()))?,
            blockchain: blockchain.clone(),
            _guard: SnapshotGuard::new(Arc::clone(&snapshots_live), latest_block_num),
        });
        let in_memory = Arc::new(ArcSwap::from(initial_snapshot));

        // Assemble the block writer. It owns the writable trees and processes write requests
        // serially, publishing a new snapshot after each committed block. The caller runs it; it
        // exits when the shutdown token is cancelled or all write handles are dropped.
        let (write_tx, write_rx) = tokio::sync::mpsc::channel(1);
        let write_handle = WriteHandle::new(write_tx);
        let block_writer = BlockWriter {
            db: Arc::clone(&db),
            block_store: Arc::clone(&block_store),
            in_memory: Arc::clone(&in_memory),
            committed_tip_tx: Arc::clone(&committed_tip_tx),
            block_cache: block_cache.clone(),
            rx: write_rx,
            shutdown,
            nullifier_tree,
            account_tree,
            blockchain,
            forest,
            snapshots_live,
        };
        let state = Self {
            data_directory: data_path.to_path_buf(),
            db,
            block_store,
            in_memory,
            write_handle,
            proven_tip,
            committed_tip_tx,
            block_cache,
            proof_cache,
        };

        Ok(LoadedState { state, writer: block_writer })
    }

    /// Returns a watch receiver that wakes every time a new block is committed.
    pub fn subscribe_committed_tip(&self) -> watch::Receiver<BlockNumber> {
        self.committed_tip_tx.subscribe()
    }

    /// Loads serialized block proving inputs from the block store.
    pub async fn load_proving_inputs(
        &self,
        block_num: BlockNumber,
    ) -> std::io::Result<Option<Vec<u8>>> {
        self.block_store.load_proving_inputs(block_num).await
    }

    /// Returns a watch receiver that wakes every time the proven-in-sequence tip advances.
    pub fn subscribe_proven_tip(&self) -> watch::Receiver<BlockNumber> {
        self.proven_tip.subscribe()
    }

    // SNAPSHOT HELPERS
    // --------------------------------------------------------------------------------------------

    /// Returns the current in-memory state snapshot (wait-free, no lock required).
    ///
    /// The returned snapshot is a frozen view: it is unaffected if the writer publishes a new
    /// snapshot while it is held.
    fn snapshot(&self) -> Arc<InMemoryState> {
        self.in_memory.load_full()
    }

    /// Runs a synchronous read-only operation over the current in-memory state snapshot on Tokio's
    /// blocking path.
    ///
    /// The account and nullifier trees may be backed by `RocksDB`, so tree access must not run on
    /// an async worker thread directly. This helper preserves the current tracing span while
    /// moving the closure body into `block_in_place`.
    fn with_inner_read_blocking<R>(&self, f: impl FnOnce(&InMemoryState) -> R) -> R {
        let span = Span::current();
        tokio::task::block_in_place(|| {
            span.in_scope(|| {
                let snapshot = self.snapshot();
                f(&snapshot)
            })
        })
    }

    /// Runs a synchronous read-only operation over the account state forest snapshot on Tokio's
    /// blocking path.
    ///
    /// See [`Self::with_inner_read_blocking`] for why this uses `block_in_place`.
    fn with_forest_read_blocking<R>(
        &self,
        f: impl FnOnce(&AccountStateForest<AccountStateForestBackendReader>) -> R,
    ) -> R {
        self.with_inner_read_blocking(|snapshot| f(&snapshot.forest))
    }

    // STATE ACCESSORS
    // --------------------------------------------------------------------------------------------

    /// Queries a [BlockHeader] from the database, and returns it alongside its inclusion proof.
    ///
    /// If [None] is given as the value of `block_num`, the data for the latest [BlockHeader] is
    /// returned.
    #[miden_instrument(
        level = "debug",
        target = COMPONENT,
        skip_all,
        err,
    )]
    pub async fn get_block_header(
        &self,
        block_num: Option<BlockNumber>,
        include_mmr_proof: bool,
    ) -> Result<(Option<BlockHeader>, Option<MmrProof>), GetBlockHeaderError> {
        // Resolve "latest" against the in-memory snapshot rather than the DB: mid-apply, the DB may
        // already contain a block that the snapshot's blockchain cannot prove yet. Scoping the DB
        // query by the snapshot's tip keeps the header and MMR proof consistent.
        let snapshot = self.snapshot();
        let latest_block_num = snapshot.latest_block_num();
        let block_num = block_num.unwrap_or(latest_block_num);
        if block_num > latest_block_num {
            return Ok((None, None));
        }

        let block_header = self.db.select_block_header_by_block_num(Some(block_num)).await?;
        if let Some(header) = block_header {
            let mmr_proof = if include_mmr_proof {
                let mmr_proof = snapshot.blockchain.open(header.block_num())?;
                Some(mmr_proof)
            } else {
                None
            };
            Ok((Some(header), mmr_proof))
        } else {
            Ok((None, None))
        }
    }

    /// Queries a list of notes from the database.
    ///
    /// If the provided list of [`NoteId`] given is empty or no note matches the provided
    /// [`NoteId`] an empty list is returned.
    pub async fn get_notes_by_id(
        &self,
        note_ids: Vec<NoteId>,
    ) -> Result<Vec<NoteRecord>, DatabaseError> {
        self.db.select_notes_by_id(note_ids).await
    }

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
            .db
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

        let (batch_reference_block, partial_mmr) = {
            let snapshot = self.snapshot();

            let latest_block_num = snapshot.latest_block_num();

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
            // - The latest block num was retrieved from the inner blockchain from which we will
            //   also retrieve the proofs, so it is guaranteed to exist in that chain.
            // - We have checked that no block number in the blocks set is greater than latest block
            //   number *and* latest block num was removed from the set. Therefore only block
            //   numbers smaller than latest block num remain in the set. Therefore all the block
            //   numbers are guaranteed to exist in the chain state at latest block num.
            let partial_mmr = snapshot
                .blockchain
                .partial_mmr_from_blocks(&blocks, latest_block_num)
                .expect("latest block num should exist and all blocks in set should be < than latest block");

            (latest_block_num, partial_mmr)
        };

        // Fetch the reference block of the batch as part of this query, so we can avoid looking it
        // up in a separate DB access.
        let mut headers = self
            .db
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
            .db
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
            .db
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
    ///
    /// This method acquires the lock to the inner state and does not access the DB so we release
    /// the lock asap.
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

            ControlFlow::Continue((
                account_commitment,
                nullifiers,
                new_account_id_prefix_is_unique,
                inner.latest_block_num(),
            ))
        });
        // `Break` carries a complete response (duplicate account ID prefix), so it is returned
        // as-is without the note lookup below; `Continue` carries the tree reads needed to build
        // the full response.
        let (account_commitment, nullifiers, new_account_id_prefix_is_unique, latest_block_num) =
            match tree_inputs {
                ControlFlow::Continue(inputs) => inputs,
                ControlFlow::Break(response) => return Ok(response),
            };

        // Scope the note lookup by the snapshot's tip so the result is consistent with the tree
        // reads above: mid-apply, the DB may already contain notes from a block the snapshot does
        // not include yet.
        let found_unauthenticated_notes = self
            .db
            .select_existing_note_commitments(unauthenticated_note_commitments, latest_block_num)
            .await?;

        Ok(TransactionInputs {
            account_commitment,
            nullifiers,
            found_unauthenticated_notes,
            new_account_id_prefix_is_unique,
        })
    }

    /// Filters `account_ids` down to the subset classified as network accounts.
    pub async fn filter_network_accounts(
        &self,
        account_ids: &[AccountId],
    ) -> Result<HashSet<AccountId>, DatabaseError> {
        self.db.select_network_accounts_subset(account_ids.to_vec()).await
    }

    /// Returns the effective chain tip for the given finality level.
    ///
    /// - [`Finality::Committed`]: returns the latest committed block number (from the in-memory
    ///   snapshot).
    /// - [`Finality::Proven`]: returns the latest proven-in-sequence block number (cached via watch
    ///   channel, updated by the proof scheduler).
    pub fn chain_tip(&self, finality: Finality) -> BlockNumber {
        match finality {
            Finality::Committed => self.snapshot().latest_block_num(),
            Finality::Proven => self.proven_tip.read(),
        }
    }

    /// Loads a block from the in-memory replica cache or block store. Return `Ok(None)` if the
    /// block is not found.
    pub async fn load_block(
        &self,
        block_num: BlockNumber,
    ) -> Result<Option<Vec<u8>>, DatabaseError> {
        if block_num > self.chain_tip(Finality::Committed) {
            return Ok(None);
        }
        if let Some(block) = self.block_cache.get(block_num) {
            return Ok(Some(block.block_bytes().to_vec()));
        }
        self.block_store.load_block(block_num).await.map_err(Into::into)
    }

    /// Loads a block proof from the in-memory replica cache or block store. Returns `Ok(None)` if
    /// the proof is not found.
    pub async fn load_proof(
        &self,
        block_num: BlockNumber,
    ) -> Result<Option<Vec<u8>>, DatabaseError> {
        if block_num > self.chain_tip(Finality::Proven) {
            return Ok(None);
        }
        if let Some(proof) = self.proof_cache.get(block_num) {
            return Ok(Some(proof.proof_bytes().to_vec()));
        }
        self.block_store.load_proof(block_num).await.map_err(Into::into)
    }

    /// Returns the script for a note by its root.
    pub async fn get_note_script_by_root(
        &self,
        root: Word,
    ) -> Result<Option<NoteScript>, DatabaseError> {
        self.db.select_note_script_by_root(root).await
    }
}
