mod client;
mod server;

pub use client::{ApiClient, MetadataInterceptor};
pub use server::Rpc;

// CONSTANTS
// =================================================================================================
pub const COMPONENT: &str = "miden-rpc";
