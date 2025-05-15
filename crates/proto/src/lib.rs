mod client;
pub mod domain;
pub mod errors;

#[rustfmt::skip]
pub mod generated;

// RE-EXPORTS
// ================================================================================================

pub use client::{AcceptHeaderInterceptor, RpcClient, connect};
pub use domain::{
    account::{AccountState, AccountWitnessRecord},
    convert,
    nullifier::NullifierWitnessRecord,
    try_convert,
};
