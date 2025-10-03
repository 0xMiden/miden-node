use crate::errors::GrpcError;
use crate::grpc_error;

// Submit proven transaction error codes for gRPC Status::details
// =================================================================================================

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum SubmitProvenTransactionGrpcError {
    Internal = 0,
    DeserializationFailed = 1,
    InvalidTransactionProof = 2,
    IncorrectAccountInitialCommitment = 3,
    InputNotesAlreadyConsumed = 4,
    UnauthenticatedNotesNotFound = 5,
    OutputNotesAlreadyExist = 6,
    TransactionExpired = 7,
}

grpc_error!(SubmitProvenTransactionGrpcError);

// Submit proven batch error codes for gRPC Status::details
// =================================================================================================

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum SubmitProvenBatchGrpcError {
    #[allow(dead_code)]
    Internal = 0,
    DeserializationFailed = 1,
}

grpc_error!(SubmitProvenBatchGrpcError);
