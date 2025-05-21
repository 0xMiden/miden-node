mod api;
mod interceptor;

pub use api::RpcClient;
pub use interceptor::MetadataInterceptor;

// CONSTANTS
// =================================================================================================
pub const COMPONENT: &str = "miden-rpc-client";
