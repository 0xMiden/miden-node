//! Store lifecycle: loading the state, starting its block writer, and stopping the store.

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
use tokio::sync::watch;

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
use crate::state::writer::{BlockWriter, WriteHandle};
use crate::state::{BlockCache, InMemoryState, ProofCache, SnapshotGuard, State};
use crate::{COMPONENT, DataDirectory, DatabaseOptions};

/// Number of recent committed blocks held in the in-memory cache for replica subscriptions.
const BLOCK_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(512).unwrap();

/// Number of recent block proofs held in the in-memory cache for replica subscriptions.
const PROOF_CACHE_CAPACITY: NonZeroUsize = NonZeroUsize::new(512).unwrap();

// LOADED STATE
// ================================================================================================

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
