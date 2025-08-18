//! Remote prover server implementations
//!
//! Provides gRPC server, proxy, and worker management functionality.

pub mod api;
pub mod commands;
pub mod error;
pub mod proxy;
pub mod utils;

// Re-export key types
pub use api::ProofType;
pub use error::RemoteProverError;

#[cfg(feature = "server")]
pub use crate::generated::server::*;
