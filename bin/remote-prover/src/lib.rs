//! Miden Remote Prover
//!
//! This crate provides both client and server functionality for Miden's remote proving service.
//!
//! ## Features
//!
//! - `client`: Enable client functionality (`RemoteTransactionProver`, etc.)
//! - `server`: Enable server functionality (binary and library)
//! - `tx-prover`: Enable transaction proving client
//! - `batch-prover`: Enable batch proving client
//! - `block-prover`: Enable block proving client
//! - `std`: Enable standard library support
//! - `concurrent`: Enable concurrent execution

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(all(not(feature = "std"), feature = "client"))]
extern crate alloc;

// Component identifier for structured logging and tracing
pub const COMPONENT: &str = "miden-remote-prover";

// Conditional module exports based on features
#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "server")]
pub mod server;

pub mod generated;
pub mod shared;

// Utils is now part of the server module

// Re-exports for backward compatibility
#[cfg(feature = "client")]
pub mod remote_prover {
    pub use crate::client::*;
}

// Server-side re-exports
#[cfg(feature = "server")]
pub use server::{api, error};
#[cfg(feature = "client")]
pub use shared::error::RemoteProverClientError;
