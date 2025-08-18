//! Remote prover client implementations
//!
//! Provides gRPC clients for interacting with remote proving services.

// Conditional exports based on enabled features
#[cfg(feature = "tx-prover")]
pub mod tx_prover;
#[cfg(feature = "tx-prover")]
pub use tx_prover::RemoteTransactionProver;

#[cfg(feature = "batch-prover")]
pub mod batch_prover;
#[cfg(feature = "batch-prover")]
pub use batch_prover::RemoteBatchProver;

#[cfg(feature = "block-prover")]
pub mod block_prover;
#[cfg(feature = "block-prover")]
pub use block_prover::RemoteBlockProver;

// Generated protobuf code
pub mod generated {
    #[cfg(feature = "std")]
    pub use crate::generated::proto::*;
    #[cfg(not(feature = "std"))]
    pub use crate::generated::proto::*;
}

// Re-export common error type for backward compatibility
// pub use RemoteProverClientError;
