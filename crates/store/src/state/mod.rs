//! Abstraction to synchronize state modifications.
//!
//! The [State] provides data access and modifications methods, its main purpose is to ensure that
//! data is atomically written, and that reads are consistent.

use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;
use miden_protocol::block::BlockNumber;
use tokio::sync::watch;

use crate::blocks::BlockStore;
use crate::db::Db;
use crate::proven_tip::ProvenTipWriter;

mod loader;

mod replica;
pub use replica::{BlockCache, BlockNotification, ProofCache, ProofNotification};

mod account;

mod apply_block;
mod apply_proof;
mod block_lifecycle;
mod bootstrap;
mod disk_monitor;
mod sync_state;

mod inputs;
pub use inputs::TransactionInputs;

mod lifecycle;
pub use lifecycle::{LoadedState, WriterTask};

mod queries;

mod snapshot;
pub(crate) use snapshot::{InMemoryState, SNAPSHOTS_LIVE_WARN_THRESHOLD, SnapshotGuard};

mod writer;
use writer::WriteHandle;

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
    /// Readers load the snapshot wait-free via [`ArcSwap::load_full`]; the
    /// [`BlockWriter`](writer::BlockWriter) task atomically replaces the pointer after each
    /// committed block. Readers holding an old snapshot are unaffected by the swap.
    in_memory: Arc<ArcSwap<InMemoryState>>,

    /// Handle for sending block-write requests to the [`BlockWriter`](writer::BlockWriter) task.
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
