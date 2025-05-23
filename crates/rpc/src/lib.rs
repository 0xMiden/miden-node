mod client;
mod server;
#[cfg(test)]
mod tests;

pub use client::{ApiClient, MetadataInterceptor};
pub use server::Rpc;

// CONSTANTS
// =================================================================================================
pub const COMPONENT: &str = "miden-rpc";
