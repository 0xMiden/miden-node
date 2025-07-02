mod builder;
use std::num::NonZeroUsize;

pub use builder::NetworkTransactionBuilder;
pub(crate) mod state;
pub(crate) mod store;

// CONSTANTS
// =================================================================================================

pub(crate) const COMPONENT: &str = "miden-ntx-builder";

/// Maximum number of network notes a network transaction is allowed to consume.
pub(crate) const MAX_NOTES_PER_TX: NonZeroUsize = NonZeroUsize::new(50).unwrap();

/// Maximum number of network transactions which should be in progress concurrently.
///
/// This only counts transactions which are being computed locally and does not include
/// uncommitted transactions in the mempool.
pub(crate) const MAX_IN_PROGRESS_TXS: usize = 4;
