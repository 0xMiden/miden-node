use miden_block_prover::ProvenBlockError;
use miden_node_proto::errors::ConversionError;
use miden_node_utils::{ErrorReport, formatting::format_opt};
use miden_objects::{
    ProposedBatchError, ProposedBlockError, ProvenBatchError, Word,
    account::AccountId,
    block::BlockNumber,
    note::{NoteId, Nullifier},
    transaction::TransactionId,
};
use miden_remote_prover_client::RemoteProverClientError;
use thiserror::Error;
use tokio::task::JoinError;

// Block-producer errors
// =================================================================================================

#[derive(Debug, Error)]
pub enum BlockProducerError {
    /// A block-producer task completed although it should have ran indefinitely.
    #[error("task {task} completed unexpectedly")]
    TaskFailedSuccessfully { task: &'static str },

    /// A block-producer task panic'd.
    #[error("error joining {task} task")]
    JoinError { task: &'static str, source: JoinError },

    /// A block-producer task reported a transport error.
    #[error("task {task} had a transport error")]
    TonicTransportError {
        task: &'static str,
        source: anyhow::Error,
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

    /// The account's initial commitment did not match the current account's commitment
    #[error("transaction's initial state commitment {tx_initial_account_commitment} does not match the account's current value of {}", format_opt(.current_account_commitment.as_ref()))]
    IncorrectAccountInitialCommitment {
        tx_initial_account_commitment: Word,
        current_account_commitment: Option<Word>,
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
                | VerifyTxError::IncorrectAccountInitialCommitment { .. }
                | VerifyTxError::InvalidTransactionProof(_),
            )
            | AddTransactionError::Expired { .. }
            | AddTransactionError::TransactionDeserializationFailed(_) => {
                Self::invalid_argument(value.as_report())
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
    RemoteProverClientError(#[source] RemoteProverClientError),

    #[error("batch proof security level is too low: {0} < {1}")]
    SecurityLevelTooLow(u32, u32),
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
    RemoteProverClientError(#[source] RemoteProverClientError),
    #[error("block proof security level is too low: {0} < {1}")]
    SecurityLevelTooLow(u32, u32),
}

// Store errors
// =================================================================================================

/// Errors returned by the [`StoreClient`](crate::store::StoreClient).
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("account Id prefix already exists: {0}")]
    DuplicateAccountIdPrefix(AccountId),
    #[error("gRPC client error")]
    GrpcClientError(Box<tonic::Status>),
    #[error("malformed response from store: {0}")]
    MalformedResponse(String),
    #[error("failed to parse response")]
    DeserializationError(#[from] ConversionError),
}

impl From<tonic::Status> for StoreError {
    fn from(value: tonic::Status) -> Self {
        StoreError::GrpcClientError(value.into())
    }
}
