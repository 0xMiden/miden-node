use std::time::Duration;

mod blocks;
mod db;
mod errors;
pub mod genesis;
mod server;
mod state;

pub use genesis::GenesisState;
pub use server::{DataDirectory, Store};

// CONSTANTS
// =================================================================================================
const COMPONENT: &str = "miden-store";

/// How often to run the database maintenance routine.
const DATABASE_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
