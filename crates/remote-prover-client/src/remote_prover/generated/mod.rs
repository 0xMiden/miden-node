#![allow(clippy::pedantic, reason = "generated by build.rs and tonic")]

#[cfg(all(feature = "std", target_arch = "wasm32"))]
compile_error!("The `std` feature cannot be used when targeting `wasm32`.");

#[cfg(feature = "std")]
mod std;
#[cfg(feature = "std")]
pub use std::remote_prover::*;

#[cfg(not(feature = "std"))]
mod nostd;
#[cfg(not(feature = "std"))]
pub use nostd::remote_prover::*;
