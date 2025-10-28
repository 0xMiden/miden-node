use std::time::Duration;

mod accounts;
mod blocks;
mod db;
mod errors;
pub mod genesis;
mod server;
pub mod state;

#[cfg(feature = "rocksdb")]
pub use accounts::PersistentAccountTree;
pub use accounts::{AccountTreeStorage, AccountTreeWithHistory, InMemoryAccountTree};
pub use genesis::GenesisState;
pub use server::{DataDirectory, Store};

// CONSTANTS
// =================================================================================================
const COMPONENT: &str = "miden-store";

/// How often to run the database maintenance routine.
const DATABASE_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
