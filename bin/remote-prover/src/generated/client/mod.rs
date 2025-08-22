//! Client-side generated protobuf code module
//!
//! This module provides conditional access to client protobuf definitions
//! based on the std/no_std environment.

#[cfg(feature = "std")]
mod std;

#[cfg(not(feature = "std"))]
mod nostd;

// Re-export based on current environment
#[cfg(feature = "std")]
pub use std::*;

#[cfg(not(feature = "std"))]
pub use nostd::*;

// API client type alias based on environment and target
#[cfg(target_arch = "wasm32")]
pub type ApiClient = remote_prover::api_client::ApiClient<tonic_web_wasm_client::Client>;

#[cfg(not(target_arch = "wasm32"))]
pub type ApiClient = remote_prover::api_client::ApiClient<tonic::transport::Channel>;


