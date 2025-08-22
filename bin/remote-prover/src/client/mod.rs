//! Remote prover client implementations
//!
//! Provides gRPC clients for interacting with remote proving services.

mod tx_prover;
pub use tx_prover::RemoteTransactionProver;

mod batch_prover;
pub use batch_prover::RemoteBatchProver;

mod block_prover;
pub use block_prover::RemoteBlockProver;

// Generated protobuf code
pub use crate::generated::proto as generated;
