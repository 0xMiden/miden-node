//! Live chain-tip queries and tip subscriptions.
//!
//! Both tips are published through watch channels by their single writers (the block writer for
//! the committed tip, the proof scheduler or proof sync for the proven tip). Everything here
//! reads or subscribes to those channels; none of it touches state snapshots, so these values
//! advance independently of any [`StateView`](super::StateView).

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
    /// The committed tip is published after the corresponding state snapshot, so it never reports
    /// a block that a freshly created view cannot serve.
    pub fn committed_tip(&self) -> BlockNumber {
        *self.committed_tip_tx.borrow()
    }

    /// Returns the latest block number proven in an unbroken sequence from genesis.
    ///
    /// This is a live value published by the proof scheduler (sequencer mode) or the proof sync
    /// loop (full-node mode); it is always at or behind the committed tip.
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
