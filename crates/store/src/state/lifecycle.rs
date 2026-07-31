//! Store lifecycle: loading the state, starting its write worker, and stopping the store.

use std::future::Future;
use std::num::NonZeroUsize;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::task::{Context, Poll};

use arc_swap::ArcSwap;
use miden_node_utils::ErrorReport;
use miden_node_utils::clap::StorageOptions;
use miden_node_utils::shutdown::CancellationToken;
use miden_node_utils::tracing::miden_instrument;
use miden_protocol::block::BlockNumber;
use tokio::sync::{mpsc, watch};

use crate::account_state_forest::AccountStateForestBackend;
use crate::accounts::AccountTreeWithHistory;
use crate::blocks::BlockStore;
use crate::db::Db;
use crate::errors::StateInitializationError;
use crate::proven_tip::ProvenTipWriter;
use crate::state::loader::{
    ACCOUNT_STATE_FOREST_STORAGE_DIR,
    ACCOUNT_TREE_STORAGE_DIR,
    AccountForestLoader,
    NULLIFIER_TREE_STORAGE_DIR,
    TreeStorage,
    TreeStorageLoader,
    load_mmr,
    verify_account_state_forest_consistency,
    verify_tree_consistency,
};
use crate::state::writer::{WriteRequest, WriteWorker};
use crate::state::{BlockCache, ProofCache, SnapshotGuard, State, StateSnapshot};
use crate::{COMPONENT, DataDirectory, DatabaseOptions};

/// Number of recent committed blocks held in the in-memory cache for replica subscriptions.
const BLOCK_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(512).unwrap();

/// Number of recent block proofs held in the in-memory cache for replica subscriptions.
const PROOF_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(512).unwrap();

// LOADED STATE
// ================================================================================================

/// A loaded store state whose write worker has not been started yet.
///
/// Returned by [`State::load`]. [`Self::start`] spawns the writer and yields the read-only
/// [`State`] together with the write capabilities; since this is the only way to obtain a
/// [`BlockWriter`], [`BlockWriter::apply_block`] can always make progress.
#[must_use = "call `start` to spawn the write worker and obtain the state"]
pub struct LoadedState {
    state: State,
    writer: WriteWorker,
    write_tx: mpsc::Sender<WriteRequest>,
}

impl LoadedState {
    /// Spawns the write worker onto the current runtime and returns the read-only state together
    /// with the write capabilities and the writer task's handle.
    ///
    /// [`Arc<State>`] is the read-only view shared with every component that queries or
    /// subscribes; it exposes no mutating methods. [`BlockWriter`] and [`ProofWriter`] are the
    /// only handles able to mutate the store — hand them to the single task driving each write
    /// path (block production or sync, and proof scheduling or sync respectively).
    ///
    /// The writer exits once the shutdown token passed to [`State::load`] is cancelled or the
    /// [`BlockWriter`] (holding the only request sender) is dropped — an in-flight block write
    /// always completes first. Awaiting the returned handle after either event guarantees the
    /// writer has released the tree storage it owns; a join error carries a writer panic.
    ///
    /// Callers without a token to cancel should stop the store via [`BlockWriter::stop`] rather
    /// than dropping and joining by hand.
    pub fn start(self) -> (Arc<State>, BlockWriter, ProofWriter, WriterTask) {
        let writer_task = tokio::spawn(self.writer.run());
        let state = Arc::new(self.state);
        let block_writer = BlockWriter {
            state: Arc::clone(&state),
            write_tx: self.write_tx,
        };
        let proof_writer = ProofWriter { state: Arc::clone(&state) };
        (state, block_writer, proof_writer, WriterTask(writer_task))
    }
}

// WRITE CAPABILITIES
// ================================================================================================

/// The store's block-write capability.
///
/// Only handle able to apply blocks; obtained exactly once from [`LoadedState::start`] and
/// deliberately not cloneable, so granting it to a single task (the block builder in sequencer
/// mode, the block sync loop in full-node mode) statically prevents every other component from
/// writing blocks. Dereferences to [`State`] for read access.
pub struct BlockWriter {
    state: Arc<State>,
    /// Sender for block-write requests to the [`WriteWorker`](crate::state::writer::WriteWorker)
    /// task. Never cloned out of this struct: the writer exits once it is dropped.
    pub(super) write_tx: mpsc::Sender<WriteRequest>,
}

impl std::ops::Deref for BlockWriter {
    type Target = State;

    fn deref(&self) -> &State {
        &self.state
    }
}

impl BlockWriter {
    /// Returns the shared read-only state.
    pub fn state(&self) -> &Arc<State> {
        &self.state
    }

    /// Stops the store, waiting until the write worker has released the tree storage it owns.
    ///
    /// Consumes the capability — closing the write channel the write worker listens on — and then
    /// joins the writer task returned by [`LoadedState::start`]. The drop must precede the join
    /// or the write worker never observes the closed channel; doing both here keeps that ordering
    /// out of caller hands. Read-only [`State`] references may outlive the stop.
    ///
    /// Callers that need the storage released deterministically must use this method instead of
    /// dropping: the node's `recover` command stops the store before the process exits, and the
    /// stress-test's store seeding stops it so the same data directory can be re-loaded (or its
    /// temporary directory deleted) immediately afterwards. The running node does not use this
    /// method — its writer exits via the shutdown token passed to [`State::load`] and is joined
    /// through the node's task set.
    ///
    /// # Panics
    ///
    /// Panics if the writer task panicked.
    pub async fn stop(self, writer_task: WriterTask) {
        drop(self);
        writer_task.await.expect("write worker task should not panic");
    }
}

/// The store's proof-write capability.
///
/// Only handle able to commit block proofs and advance the proven tip; obtained exactly once from
/// [`LoadedState::start`] and deliberately not cloneable, so granting it to a single task (the
/// proof scheduler in sequencer mode, the proof sync loop in full-node mode) statically prevents
/// every other component from writing proofs. Dereferences to [`State`] for read access.
pub struct ProofWriter {
    state: Arc<State>,
}

impl std::ops::Deref for ProofWriter {
    type Target = State;

    fn deref(&self) -> &State {
        &self.state
    }
}

impl ProofWriter {
    /// Returns the shared read-only state.
    pub fn state(&self) -> &Arc<State> {
        &self.state
    }
}

// WRITER TASK
// ================================================================================================

/// Handle of the store's write worker task, returned by [`LoadedState::start`].
///
/// Awaiting it resolves once the writer has exited and released the tree storage it owns; a join
/// error carries a writer panic. The newtype ensures [`BlockWriter::stop`] can only be given the
/// store's own writer task, and deliberately does not expose [`tokio::task::JoinHandle::abort`]:
/// aborting the writer mid-write could leave the trees lagging the committed database state,
/// voiding the guarantee that an in-flight block write always completes.
#[must_use = "await the writer task to observe its exit, or pass it to `BlockWriter::stop`"]
pub struct WriterTask(tokio::task::JoinHandle<()>);

impl Future for WriterTask {
    type Output = Result<(), tokio::task::JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx)
    }
}

// LOAD & STOP
// ================================================================================================

impl State {
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
        let initial_snapshot = Arc::new(StateSnapshot::new(
            nullifier_tree
                .reader()
                .map_err(|e| StateInitializationError::NullifierTreeIoError(e.as_report()))?,
            blockchain.clone(),
            account_tree.reader(),
            forest
                .reader()
                .map_err(|e| StateInitializationError::AccountStateForestIoError(e.as_report()))?,
            SnapshotGuard::new(Arc::clone(&snapshots_live), latest_block_num),
        ));
        let in_memory = Arc::new(ArcSwap::from(initial_snapshot));

        // Assemble the write worker. It owns the writable trees and processes write requests
        // serially, publishing a new snapshot after each committed block. The caller runs it; it
        // exits when the shutdown token is cancelled or the `BlockWriter` (holding the only request
        // sender) is dropped.
        let (write_tx, write_rx) = mpsc::channel(1);
        let block_writer = WriteWorker {
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
            proven_tip,
            committed_tip_tx,
            block_cache,
            proof_cache,
        };

        Ok(LoadedState { state, writer: block_writer, write_tx })
    }

    /// Loads the state with default options and starts its write worker, detaching the worker
    /// task.
    ///
    /// Test-only helper for tests in sibling crates that don't manage the writer's lifecycle:
    /// the detached writer exits once the returned [`BlockWriter`] is dropped. Hidden from public
    /// docs and not part of the stable API.
    ///
    /// # Panics
    ///
    /// Panics if the state fails to load.
    #[doc(hidden)]
    pub async fn for_tests(data_path: &Path) -> (Arc<Self>, BlockWriter, ProofWriter) {
        let (state, block_writer, proof_writer, _writer_task) =
            Self::load(data_path, StorageOptions::default(), CancellationToken::new())
                .await
                .expect("state should load")
                .start();
        (state, block_writer, proof_writer)
    }
}
