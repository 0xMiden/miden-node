mod client;
mod server;
#[cfg(test)]
mod tests;

pub use client::{Builder, Rpc as RpcClientMarker, RpcClient};
pub use server::Rpc;

// CONSTANTS
// =================================================================================================
pub const COMPONENT: &str = "miden-rpc";
