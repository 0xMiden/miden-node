use miden_block_prover::ProvenBlockError;
use miden_node_proto::errors::ConversionError;
use miden_node_proto::generated::errors::SubmitProvenTransactionError;
use miden_node_utils::ErrorReport;
use miden_objects::account::AccountId;
use miden_objects::block::BlockNumber;
use miden_objects::note::{NoteId, Nullifier};
use miden_objects::transaction::TransactionId;
use miden_objects::{ProposedBatchError, ProposedBlockError, ProvenBatchError, Word};
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
    #[error(
        "transaction's initial state commitment {tx_initial_account_commitment} does not match the account's current value of {current_account_commitment}"
    )]
    IncorrectAccountInitialCommitment {
        tx_initial_account_commitment: Word,
        current_account_commitment: Word,
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

// Error codes for gRPC Status::details
// =================================================================================================

impl AddTransactionError {
    fn api_error(&self) -> SubmitProvenTransactionError {
        match self {
            AddTransactionError::VerificationFailed(tx_verify_error) => match tx_verify_error {
                VerifyTxError::InputNotesAlreadyConsumed(_) => {
                    SubmitProvenTransactionError::InputNotesAlreadyConsumed
                },
                VerifyTxError::StoreConnectionFailed(_) => {
                    SubmitProvenTransactionError::InternalError
                },
                VerifyTxError::UnauthenticatedNotesNotFound(_) => {
                    SubmitProvenTransactionError::UnauthenticatedNotesNotFound
                },
                VerifyTxError::OutputNotesAlreadyExist(_) => {
                    SubmitProvenTransactionError::OutputNotesAlreadyExist
                },
                VerifyTxError::IncorrectAccountInitialCommitment { .. } => {
                    SubmitProvenTransactionError::IncorrectAccountInitialCommitment
                },
                VerifyTxError::InvalidTransactionProof(_) => {
                    SubmitProvenTransactionError::InvalidTransactionProof
                },
            },
            AddTransactionError::StaleInputs { .. } => SubmitProvenTransactionError::InternalError,
            AddTransactionError::Expired { .. } => SubmitProvenTransactionError::TransactionExpired,
            AddTransactionError::TransactionDeserializationFailed(_) => {
                SubmitProvenTransactionError::DeserializationFailed
            },
        }
    }
}

impl From<AddTransactionError> for tonic::Status {
    fn from(value: AddTransactionError) -> Self {
        let api_error = value.api_error();

        let message = if api_error.is_internal() {
            "Internal error".to_owned()
        } else {
            value.as_report()
        };

        tonic::Status::with_details(
            api_error.tonic_code(),
            message,
            // Details are serialized as a single byte containing the error code value.
            // Clients can decode this by reading the first byte of the details field.
            // Example: details[0] will contain the SubmitProvenTransactionError enum value (0-8)
            vec![api_error.api_code()].into(),
        )
    }
}

// Submit proven batch by user errors
// =================================================================================================

#[derive(Debug, Error)]
pub enum SubmitProvenBatchError {
    #[error("batch deserialization failed")]
    Deserialization(#[source] miden_objects::utils::DeserializationError),
}

impl From<SubmitProvenBatchError> for tonic::Status {
    fn from(value: SubmitProvenBatchError) -> Self {
        match value {
            SubmitProvenBatchError::Deserialization(_) => Self::invalid_argument(value.as_report()),
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
