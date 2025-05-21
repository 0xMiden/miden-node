#![no_std]

#[macro_use]
extern crate alloc;

mod api;
mod interceptor;

pub use api::RpcClient;
pub use interceptor::MetadataInterceptor;

// CONSTANTS
// =================================================================================================
pub const COMPONENT: &str = "miden-rpc-client";
