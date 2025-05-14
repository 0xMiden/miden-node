use std::time::Duration;

mod blocks;
mod db;
mod errors;
mod genesis;
mod server;
mod state;

pub use genesis::GenesisState;
pub use server::{DataDirectory, Store};

// CONSTANTS
// =================================================================================================
const COMPONENT: &str = "miden-store";

/// Number of sql statements that each connection will cache.
const SQL_STATEMENT_CACHE_CAPACITY: usize = 32;

/// How often to run the database maintenance routine.
const DATABASE_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
