//! Live chain-tip queries and tip subscriptions.
//!
//! Both tips are published through watch channels by their single writers (the block writer for
//! the committed tip, the proof scheduler or proof sync for the proven tip). Everything here
//! reads or subscribes to those channels; none of it touches state snapshots, so these values
//! advance independently of any [`StateView`](super::StateView).

use miden_protocol::block::BlockNumber;
use tokio::sync::watch;

use super::State;

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

// TIP QUERIES & SUBSCRIPTIONS
// ================================================================================================

impl State {
    /// Returns the effective chain tip for the given finality level.
    ///
    /// This is a live value: it advances independently of any [`StateView`](super::StateView).
    /// Reads that must be consistent with data must use a view's tip instead.
    ///
    /// The committed tip is published after the corresponding state snapshot, so it never reports
    /// a block that a freshly created view cannot serve.
    pub fn chain_tip(&self, finality: Finality) -> BlockNumber {
        match finality {
            Finality::Committed => *self.committed_tip_tx.borrow(),
            Finality::Proven => self.proven_tip.read(),
        }
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
