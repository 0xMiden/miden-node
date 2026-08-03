//! Live chain-tip queries and tip subscriptions, deliberately kept off
//! [`StateView`](super::StateView).
//!
//! Both tips are published through watch channels by their single writers (the block writer for
//! the committed tip, the proof scheduler or proof sync for the proven tip). Everything here
//! reads or subscribes to those channels; none of it touches state snapshots — trees, forest, or
//! DB — so these values advance independently of any `StateView`.
//!
//! This lives on [`State`] rather than `StateView` on purpose, not just because the values are
//! live. A `StateView` pins a whole `StateSnapshot` (including the `RocksDB` snapshots backing
//! the trees) and is meant to be released as soon as possible; a tip read never needs that
//! snapshot, so routing it through a view would pin one for no reason. The `subscribe_*`
//! receivers go further: they are meant to be held for the lifetime of a long-running task (a
//! proof-sync loop, a subscription stream), which is the opposite of a `StateView`'s
//! request-scoped, drop-it-immediately lifetime — so they could not live on `StateView` even if
//! the values themselves needed one.

use miden_protocol::block::BlockNumber;
use tokio::sync::watch;

use super::State;

// TIP QUERIES & SUBSCRIPTIONS
// ================================================================================================

impl State {
    /// Returns the latest committed (but not necessarily proven) block number.
    ///
    /// This is a live value: it advances independently of any [`StateView`](super::StateView).
    /// Reads that must be consistent with data must use a view's tip instead.
    ///
    /// Ordering matters when combining this with a view: read the tip *before* creating the view.
    /// Tips are monotonic and the committed tip is published after its snapshot, so a view
    /// created afterwards can always serve a tip read earlier. Reading a live tip *after*
    /// creating a view is racy — the tip may have advanced past the view's pinned snapshot.
    pub fn committed_tip(&self) -> BlockNumber {
        *self.committed_tip_tx.borrow()
    }

    /// Returns the latest block number proven in an unbroken sequence from genesis.
    ///
    /// This is a live value published by the proof scheduler (sequencer mode) or the proof sync
    /// loop (full-node mode); it is always at or behind the committed tip.
    ///
    /// Ordering matters when combining this with a view: read the tip *before* creating the view.
    /// Proofs are only applied to committed blocks and each committed tip is published after its
    /// snapshot, so a view created afterwards can always serve a proven tip read earlier. Reading
    /// it *after* creating a view is racy — a block may commit and prove concurrently, putting
    /// the proven tip past the view's pinned snapshot.
    pub fn proven_tip(&self) -> BlockNumber {
        self.proven_tip.read()
    }

    /// Returns a watch receiver that wakes every time a new block is committed.
    pub fn subscribe_committed_tip(&self) -> watch::Receiver<BlockNumber> {
        self.committed_tip_tx.subscribe()
    }

    /// Returns a watch receiver that wakes every time the proven-in-sequence tip advances.
    pub fn subscribe_proven_tip(&self) -> watch::Receiver<BlockNumber> {
        self.proven_tip.subscribe()
    }
}
