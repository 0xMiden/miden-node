#![no_std]

#[macro_use]
extern crate alloc;

mod api;
mod errors;
mod interceptor;

pub use api::RpcClient;
pub use errors::RpcError;
pub use interceptor::MetadataInterceptor;

// CONSTANTS
// =================================================================================================
pub const COMPONENT: &str = "miden-rpc-client";
