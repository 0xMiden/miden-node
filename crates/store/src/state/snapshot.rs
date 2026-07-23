//! In-memory snapshot machinery for lock-free reads.
//!
//! Readers access the store's tree state through immutable [`InMemoryState`] snapshots published
//! by the block writer after each committed block. [`SnapshotGuard`] tracks how many snapshot
//! generations are pinned by readers, since each generation pins a `RocksDB` snapshot.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use miden_protocol::block::nullifier_tree::NullifierTree;
use miden_protocol::block::{BlockNumber, Blockchain};
use miden_protocol::crypto::merkle::smt::LargeSmt;
use tracing::Span;

use crate::COMPONENT;
use crate::account_state_forest::{AccountStateForest, AccountStateForestBackendReader};
use crate::accounts::AccountTreeWithHistory;
use crate::state::State;
use crate::state::loader::TreeStorageReader;

/// Snapshot lifetime above which [`SnapshotGuard`] logs a warning on release.
///
/// Readers are expected to be request-scoped, so a snapshot outliving several block intervals
/// indicates a slow or leaked reader pinning a `RocksDB` snapshot (see [`SnapshotGuard`]).
const SNAPSHOT_LIFETIME_WARN_THRESHOLD: Duration = Duration::from_secs(10);

/// Number of live snapshot generations above which the block writer logs a warning after
/// publishing a new snapshot.
///
/// Steady state is 1-2 generations: the just-published snapshot plus predecessors briefly pinned
/// by in-flight requests. A sustained higher count means slow or leaked readers are holding old
/// generations alive (see [`SnapshotGuard`]).
pub(crate) const SNAPSHOTS_LIVE_WARN_THRESHOLD: u64 = 4;

// SNAPSHOT GUARD
// ================================================================================================

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
    pub(crate) fn new(live: Arc<AtomicUsize>, block_num: BlockNumber) -> Self {
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

// IN-MEMORY STATE
// ================================================================================================

/// Immutable snapshot of the in-memory tree state published after each committed block.
///
/// The trees are backed by read-only snapshot storage ([`TreeStorageReader`] /
/// [`AccountStateForestBackendReader`]), so any number of readers can access the data concurrently
/// without holding a lock and without blocking the writer.
pub(crate) struct InMemoryState {
    pub(crate) nullifier_tree: NullifierTree<LargeSmt<TreeStorageReader>>,
    pub(crate) blockchain: Blockchain,
    pub(crate) account_tree: AccountTreeWithHistory<TreeStorageReader>,
    pub(crate) forest: AccountStateForest<AccountStateForestBackendReader>,
    /// Keeps the live-snapshot count accurate; see [`SnapshotGuard`].
    pub(crate) _guard: SnapshotGuard,
}

impl InMemoryState {
    /// Returns the latest block number.
    pub(crate) fn latest_block_num(&self) -> BlockNumber {
        self.blockchain
            .chain_tip()
            .expect("chain should always have at least the genesis block")
    }
}

// SNAPSHOT HELPERS
// ================================================================================================

impl State {
    /// Returns the current in-memory state snapshot (wait-free, no lock required).
    ///
    /// The returned snapshot is a frozen view: it is unaffected if the writer publishes a new
    /// snapshot while it is held.
    pub(super) fn snapshot(&self) -> Arc<InMemoryState> {
        self.in_memory.load_full()
    }

    /// Runs a synchronous read-only operation over the current in-memory state snapshot on Tokio's
    /// blocking path.
    ///
    /// The account and nullifier trees may be backed by `RocksDB`, so tree access must not run on
    /// an async worker thread directly. This helper preserves the current tracing span while
    /// moving the closure body into `block_in_place`.
    pub(super) fn with_inner_read_blocking<R>(&self, f: impl FnOnce(&InMemoryState) -> R) -> R {
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
    pub(super) fn with_forest_read_blocking<R>(
        &self,
        f: impl FnOnce(&AccountStateForest<AccountStateForestBackendReader>) -> R,
    ) -> R {
        self.with_inner_read_blocking(|snapshot| f(&snapshot.forest))
    }
}
