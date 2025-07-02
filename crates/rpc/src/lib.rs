mod client;
mod server;
#[cfg(test)]
mod tests;

pub use client::ApiClient;
pub use server::Rpc;

// CONSTANTS
// =================================================================================================
pub const COMPONENT: &str = "miden-rpc";
