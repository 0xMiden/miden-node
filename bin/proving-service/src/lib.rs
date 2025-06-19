pub mod api;
pub mod commands;
pub mod error;
pub mod generated;
pub mod proxy;
pub mod utils;

/// Component identifier for structured logging and tracing
pub const COMPONENT: &str = "miden-proving-service";
