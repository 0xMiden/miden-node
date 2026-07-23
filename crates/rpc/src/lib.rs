mod server;
#[cfg(test)]
mod tests;

pub use server::{PreAuthSubmission, Rpc, RpcMode, SequencerInternal};

// CONSTANTS
// =================================================================================================
pub const COMPONENT: &str = "miden-rpc";
pub const LOG_TARGET: &str = "user::miden-rpc";
