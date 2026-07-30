//! Request-scoped, consistent read view of the store.
//!
//! All store reads go through [`StateView`]: it pins one in-memory snapshot for its whole
//! lifetime, and every database query it exposes is scoped by that snapshot's block height. This
//! makes it impossible to implement a read whose tree and database halves observe different chain
//! tips — mid-apply, the database may already contain rows for a block the snapshot cannot prove
//! yet.

use std::ops::RangeInclusive;
use std::sync::Arc;

use miden_protocol::block::{BlockNumber, Blockchain};
use tracing::Span;

use crate::account_state_forest::{AccountStateForest, AccountStateForestBackendReader};
use crate::db::Db;
use crate::errors::RangeBeyondTip;
use crate::state::State;
use crate::state::snapshot::StateSnapshot;

// STATE VIEW
// ================================================================================================

/// A consistent read view of the store, pinned at its snapshot's block height.
///
/// Obtained from [`State::view`]; create one per request and drop it when the request completes.
/// Holding a view pins a snapshot generation (and thereby the `RocksDB` snapshots backing the
/// trees), so it must not be stored in long-lived structs; leaked or slow readers are reported by
/// the store's snapshot-lifetime warnings.
///
/// Reads that are technically not block-scoped (e.g. content-addressed note scripts) also live
/// here so that every read path flows through a single, consistently-scoped type.
pub struct StateView {
    snapshot: Arc<StateSnapshot>,
    db: Arc<Db>,
}

impl State {
    /// Returns a read view pinned at the current chain tip (wait-free, no lock required).
    ///
    /// The view is frozen: it is unaffected if the writer publishes a new snapshot while it is
    /// held.
    pub fn view(&self) -> StateView {
        StateView {
            snapshot: self.in_memory.load_full(),
            db: Arc::clone(&self.db),
        }
    }
}

impl StateView {
    /// The chain tip this view is pinned at.
    pub fn tip(&self) -> BlockNumber {
        self.snapshot.latest_block_num()
    }

    /// Returns the pinned snapshot's blockchain MMR.
    ///
    /// The MMR is the only part of the snapshot that is purely in-memory and therefore safe to
    /// access directly on an async worker thread. The account and nullifier trees may be backed
    /// by `RocksDB` and are deliberately not reachable here — they must be accessed through
    /// [`Self::with_inner_read_blocking`].
    pub(super) fn blockchain(&self) -> &Blockchain {
        &self.snapshot.blockchain
    }

    /// Returns the database handle.
    ///
    /// Queries whose results depend on the chain tip must be scoped by [`Self::tip`] (or a block
    /// number validated against it), never by a tip obtained elsewhere.
    pub(super) fn db(&self) -> &Db {
        &self.db
    }

    /// Ensures the given block range does not extend beyond this view's chain tip.
    ///
    /// Every range-scoped read on this type calls this before touching the database, so callers
    /// never need to pre-validate ranges themselves.
    pub(super) fn check_range(
        &self,
        range: &RangeInclusive<BlockNumber>,
    ) -> Result<(), RangeBeyondTip> {
        let tip = self.tip();
        if *range.end() > tip {
            return Err(RangeBeyondTip { chain_tip: tip, block_to: *range.end() });
        }
        Ok(())
    }

    /// Runs a synchronous read-only operation over the pinned in-memory snapshot on Tokio's
    /// blocking path.
    ///
    /// The account and nullifier trees may be backed by `RocksDB`, so tree access must not run on
    /// an async worker thread directly. This helper preserves the current tracing span while
    /// moving the closure body into `block_in_place`.
    pub(super) fn with_inner_read_blocking<R>(&self, f: impl FnOnce(&StateSnapshot) -> R) -> R {
        let span = Span::current();
        tokio::task::block_in_place(|| span.in_scope(|| f(&self.snapshot)))
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
