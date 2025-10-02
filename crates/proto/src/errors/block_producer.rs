// Submit proven transaction error codes for gRPC Status::details
// =================================================================================================

use crate::errors::store::GrpcError;

#[derive(Debug, Copy, Clone)]
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

impl GrpcError for SubmitProvenTransactionGrpcError {
    fn api_code(self) -> u8 {
        self as u8
    }

    fn is_internal(&self) -> bool {
        matches!(self, SubmitProvenTransactionGrpcError::Internal)
    }
}

// Submit proven batch error codes for gRPC Status::details
// =================================================================================================

#[derive(Debug, Copy, Clone)]
#[repr(u8)]
pub enum SubmitProvenBatchGrpcError {
    #[allow(dead_code)]
    Internal = 0,
    DeserializationFailed = 1,
}

impl GrpcError for SubmitProvenBatchGrpcError {
    fn api_code(self) -> u8 {
        self as u8
    }

    fn is_internal(&self) -> bool {
        matches!(self, SubmitProvenBatchGrpcError::Internal)
    }
}
