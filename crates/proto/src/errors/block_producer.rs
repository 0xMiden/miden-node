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

impl SubmitProvenTransactionGrpcError {
    pub fn api_code(self) -> u8 {
        self as u8
    }

    pub fn is_internal(&self) -> bool {
        matches!(self, Self::Internal)
    }

    pub fn tonic_code(&self) -> tonic::Code {
        if self.is_internal() {
            tonic::Code::Internal
        } else {
            tonic::Code::InvalidArgument
        }
    }
}
