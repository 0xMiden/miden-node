use std::num::NonZeroUsize;

mod actor;
mod block_producer;
mod builder;
mod coordinator;
mod store;

pub use builder::NetworkTransactionBuilder;

// CONSTANTS
// =================================================================================================

const COMPONENT: &str = "miden-ntx-builder";

pub const DEFAULT_SCRIPT_CACHE_SIZE: NonZeroUsize = NonZeroUsize::new(1000).unwrap();

/// Maximum number of network notes a network transaction is allowed to consume.
const MAX_NOTES_PER_TX: NonZeroUsize = NonZeroUsize::new(50).unwrap();

/// Maximum number of network transactions which should be in progress concurrently.
///
/// This only counts transactions which are being computed locally and does not include
/// uncommitted transactions in the mempool.
const MAX_IN_PROGRESS_TXS: usize = 4;
