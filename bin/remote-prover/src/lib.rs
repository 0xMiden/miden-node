//! Miden Remote Prover
//!
//! This crate provides both client and server functionality for Miden's remote proving service.
//!
//! ## Features
//!
//! - `client`: Enable client functionality (all remote provers: transaction, batch, block)
//! - `server`: Enable server functionality (binary and library)
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

pub mod error;
pub mod generated;

// Utils is now part of the server module

#[cfg(feature = "client")]
pub mod remote_prover {
    pub use crate::client::*;
}

// Server-side re-exports
#[cfg(feature = "client")]
pub use error::RemoteProverClientError;
#[cfg(feature = "server")]
pub use server::api;
#[cfg(feature = "server")]
pub use server::error::RemoteProverError;
