//! Store lifecycle: loading the state, starting its write worker, and stopping the store.

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

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
use crate::state::writer::{WriteRequest, WriteWorker, WriterTask};
use crate::state::{
    BlockCache,
    BlockWriter,
    ProofCache,
    ProofWriter,
    SnapshotGuard,
    State,
    StateSnapshot,
};
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
    /// path (block production or sync, and proof scheduling or sync respectively). The
    /// capabilities expose no read access: tasks that both read and write receive the
    /// [`Arc<State>`] alongside their capability.
    ///
    /// The writer exits once `shutdown` is cancelled or the [`BlockWriter`] (holding the only
    /// request sender) is dropped — an in-flight block write always completes first. Awaiting the
    /// returned handle after either event guarantees the writer has released the tree storage it
    /// owns; a join error carries a writer panic.
    ///
    /// Callers without a token to cancel should pass [`CancellationToken::new`] and stop the store
    /// via [`BlockWriter::stop`] instead of dropping and joining by hand.
    pub fn start(
        self,
        shutdown: CancellationToken,
    ) -> (Arc<State>, BlockWriter, ProofWriter, WriterTask) {
        let writer_task = tokio::spawn(self.writer.run(shutdown));
        let state = Arc::new(self.state);
        let block_writer = BlockWriter {
            block_store: Arc::clone(&state.block_store),
            write_tx: self.write_tx,
        };
        let proof_writer = ProofWriter { state: Arc::clone(&state) };
        (state, block_writer, proof_writer, WriterTask(writer_task))
    }
}

// LOAD
// ================================================================================================

impl State {
    /// Loads the state from the data directory.
    ///
    /// The loaded state owns all store data structures and exposes subscription methods for
    /// sequencer and replica tasks. Call [`LoadedState::start`] on the result to spawn the block
    /// writer and obtain the usable [`State`].
    #[miden_instrument(
        target = COMPONENT,
    )]
    pub async fn load(
        data_path: &Path,
        storage_options: StorageOptions,
    ) -> Result<LoadedState, StateInitializationError> {
        Self::load_with_database_options(data_path, storage_options, DatabaseOptions::default())
            .await
    }

    /// Loads the state from the data directory using explicit database options.
    ///
    /// The loaded state owns all store data structures and exposes subscription methods for
    /// sequencer and replica tasks. Call [`LoadedState::start`] on the result to spawn the block
    /// writer and obtain the usable [`State`].
    #[miden_instrument(
        target = COMPONENT,
    )]
    pub async fn load_with_database_options(
        data_path: &Path,
        storage_options: StorageOptions,
        database_options: DatabaseOptions,
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

        let apply_block_thread_priority = storage_options.apply_block_thread_priority;

        #[cfg(feature = "rocksdb")]
        let (account_storage_config, nullifier_storage_config, forest_storage_config) = (
            storage_options.account_tree.into(),
            storage_options.nullifier_tree.into(),
            storage_options.account_state_forest.into(),
        );
        #[cfg(not(feature = "rocksdb"))]
        let (account_storage_config, nullifier_storage_config, forest_storage_config) =
            ((), (), ());
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
        let latest_snapshot = Arc::new(ArcSwap::from(initial_snapshot));

        // Assemble the write worker. It owns the writable trees and processes write requests
        // serially, publishing a new snapshot after each committed block. The caller runs it; it
        // exits when the shutdown token is cancelled or the `BlockWriter` (holding the only request
        // sender) is dropped.
        let (write_tx, write_rx) = mpsc::channel(1);
        let block_writer = WriteWorker::new(
            Arc::clone(&db),
            Arc::clone(&block_store),
            Arc::clone(&latest_snapshot),
            Arc::clone(&committed_tip_tx),
            block_cache.clone(),
            write_rx,
            nullifier_tree,
            account_tree,
            blockchain,
            forest,
            snapshots_live,
            apply_block_thread_priority,
        );
        let state = Self {
            data_directory: data_path.to_path_buf(),
            db,
            block_store,
            latest_snapshot,
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
            Self::load(data_path, StorageOptions::default())
                .await
                .expect("state should load")
                .start(CancellationToken::new());
        (state, block_writer, proof_writer)
    }
}
