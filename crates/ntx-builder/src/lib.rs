mod builder;
use std::num::NonZeroUsize;

pub use builder::NetworkTransactionBuilder;
pub(crate) mod state;
pub(crate) mod store;

// CONSTANTS
// =================================================================================================

pub const COMPONENT: &str = "miden-ntx-builder";

/// Maximum allowed network ntoes per network transaction.
pub(crate) const TX_NOTE_LIMIT: NonZeroUsize = NonZeroUsize::new(50).unwrap();
