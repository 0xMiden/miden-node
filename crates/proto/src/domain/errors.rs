use crate::generated::errors::{SubmitProvenBatchError, SubmitProvenTransactionError};

impl SubmitProvenTransactionError {
    pub fn api_code(&self) -> u8 {
        *self as u8
    }

    pub fn tonic_code(&self) -> tonic::Code {
        if self.is_internal() {
            tonic::Code::Internal
        } else {
            tonic::Code::InvalidArgument
        }
    }

    pub fn is_internal(&self) -> bool {
        matches!(
            self,
            SubmitProvenTransactionError::Internal | SubmitProvenTransactionError::Unspecified
        )
    }
}

impl SubmitProvenBatchError {
    pub fn api_code(&self) -> u8 {
        *self as u8
    }

    pub fn tonic_code(&self) -> tonic::Code {
        if self.is_internal() {
            tonic::Code::Internal
        } else {
            tonic::Code::InvalidArgument
        }
    }

    pub fn is_internal(&self) -> bool {
        matches!(self, SubmitProvenBatchError::Internal | SubmitProvenBatchError::Unspecified)
    }
}
