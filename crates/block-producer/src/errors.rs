use miden_block_prover::ProvenBlockError;
use miden_node_proto::errors::ConversionError;
use miden_node_utils::formatting::format_opt;
use miden_objects::{
    Digest, ProposedBatchError, ProposedBlockError,
    block::BlockNumber,
    note::{NoteId, Nullifier},
    transaction::TransactionId,
};
use miden_proving_service_client::RemoteProverError;
use miden_tx_batch_prover::errors::ProvenBatchError;
use thiserror::Error;
use tokio::task::JoinError;

// Block-producer errors
// =================================================================================================

#[derive(Debug, Error)]
pub enum BlockProducerError {
    /// A block-producer task completed although it should have ran indefinitely.
    #[error("task {task} completed unexpectedly")]
    TaskFailedSuccesfully { task: &'static str },

    /// A block-producer task panic'd.
    #[error("error joining {task} task")]
    JoinError { task: &'static str, source: JoinError },

    /// A block-producer task reported a transport error.
    #[error("task {task} had a transport error")]
    TonicTransportError {
        task: &'static str,
        source: tonic::transport::Error,
    },
}

// Transaction verification errors
// =================================================================================================

#[derive(Debug, Error)]
pub enum VerifyTxError {
    /// Another transaction already consumed the notes with given nullifiers
    #[error(
        "input notes with given nullifiers were already consumed by another transaction: {0:?}"
    )]
    InputNotesAlreadyConsumed(Vec<Nullifier>),

    /// Unauthenticated transaction notes were not found in the store or in outputs of in-flight
    /// transactions
    #[error(
        "unauthenticated transaction notes were not found in the store or in outputs of in-flight transactions: {0:?}"
    )]
    UnauthenticatedNotesNotFound(Vec<NoteId>),

    #[error("output note IDs already used: {0:?}")]
    OutputNotesAlreadyExist(Vec<NoteId>),

    /// The account's initial hash did not match the current account's hash
    #[error("incorrect account's initial hash ({tx_initial_account_hash}, current: {})", format_opt(.current_account_hash.as_ref()))]
    IncorrectAccountInitialHash {
        tx_initial_account_hash: Digest,
        current_account_hash: Option<Digest>,
    },

    /// Failed to retrieve transaction inputs from the store
    ///
    /// TODO: Make this an "internal error". Q: Should we have a single `InternalError` enum for
    /// all internal errors that can occur across the system?
    #[error("failed to retrieve transaction inputs from the store")]
    StoreConnectionFailed(#[from] StoreError),

    /// Failed to verify the transaction execution proof
    #[error("invalid transaction proof error for transaction: {0}")]
    InvalidTransactionProof(TransactionId),
}

// Transaction adding errors
// =================================================================================================

#[derive(Debug, Error)]
pub enum AddTransactionError {
    #[error("transaction verification failed")]
    VerificationFailed(#[from] VerifyTxError),

    #[error(
        "transaction input data from block {input_block} is rejected as stale because it is older than the limit of {stale_limit}"
    )]
    StaleInputs {
        input_block: BlockNumber,
        stale_limit: BlockNumber,
    },

    #[error("transaction deserialization failed")]
    TransactionDeserializationFailed(#[source] miden_objects::utils::DeserializationError),

    #[error(
        "transaction expired at block height {expired_at} but the block height limit was {limit}"
    )]
    Expired {
        expired_at: BlockNumber,
        limit: BlockNumber,
    },
}

impl From<AddTransactionError> for tonic::Status {
    fn from(value: AddTransactionError) -> Self {
        match value {
            AddTransactionError::VerificationFailed(
                VerifyTxError::InputNotesAlreadyConsumed(_)
                | VerifyTxError::UnauthenticatedNotesNotFound(_)
                | VerifyTxError::OutputNotesAlreadyExist(_)
                | VerifyTxError::IncorrectAccountInitialHash { .. }
                | VerifyTxError::InvalidTransactionProof(_),
            )
            | AddTransactionError::Expired { .. }
            | AddTransactionError::TransactionDeserializationFailed(_) => {
                Self::invalid_argument(value.to_string())
            },

            // Internal errors which should not be communicated to the user.
            AddTransactionError::VerificationFailed(VerifyTxError::StoreConnectionFailed(_))
            | AddTransactionError::StaleInputs { .. } => Self::internal("Internal error"),
        }
    }
}

// Batch building errors
// =================================================================================================

/// Error encountered while building a batch.
#[derive(Debug, Error)]
pub enum BuildBatchError {
    /// We sometimes randomly inject errors into the batch building process to test our failure
    /// responses.
    #[error("nothing actually went wrong, failure was injected on purpose")]
    InjectedFailure,

    #[error("batch proving task panic'd")]
    JoinError(#[from] tokio::task::JoinError),

    #[error("failed to fetch batch inputs from store")]
    FetchBatchInputsFailed(#[source] StoreError),

    #[error("failed to build proposed transaction batch")]
    ProposeBatchError(#[source] ProposedBatchError),

    #[error("failed to prove proposed transaction batch")]
    ProveBatchError(#[source] ProvenBatchError),

    #[error("failed to prove batch with remote prover")]
    RemoteProverError(#[source] RemoteProverError),
}

// Block building errors
// =================================================================================================

#[derive(Debug, Error)]
pub enum BuildBlockError {
    #[error("failed to apply block to store")]
    StoreApplyBlockFailed(#[source] StoreError),
    #[error("failed to get block inputs from store")]
    GetBlockInputsFailed(#[source] StoreError),
    #[error("failed to propose block")]
    ProposeBlockFailed(#[source] ProposedBlockError),
    #[error("failed to prove block")]
    ProveBlockFailed(#[source] ProvenBlockError),
    /// We sometimes randomly inject errors into the batch building process to test our failure
    /// responses.
    #[error("nothing actually went wrong, failure was injected on purpose")]
    InjectedFailure,
    #[error("failed to prove block with remote prover")]
    RemoteProverError(#[source] RemoteProverError),
}

// Store errors
// =================================================================================================

/// Errors returned by the [`StoreClient`](crate::store::StoreClient).
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("gRPC client error")]
    GrpcClientError(#[from] tonic::Status),
    #[error("malformed response from store: {0}")]
    MalformedResponse(String),
    #[error("failed to parse response")]
    DeserializationError(#[from] ConversionError),
}
