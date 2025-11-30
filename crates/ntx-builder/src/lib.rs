use std::num::NonZeroUsize;

mod block_producer;
mod builder;
mod cache;
mod state;
mod store;
mod transaction;

pub use builder::NetworkTransactionBuilder;

// CONSTANTS
// =================================================================================================

const COMPONENT: &str = "miden-ntx-builder";

/// Maximum number of network notes a network transaction is allowed to consume.
const MAX_NOTES_PER_TX: NonZeroUsize = NonZeroUsize::new(50).unwrap();

/// Maximum number of network transactions which should be in progress concurrently.
///
/// This only counts transactions which are being computed locally and does not include
/// uncommitted transactions in the mempool.
const MAX_IN_PROGRESS_TXS: usize = 4;
