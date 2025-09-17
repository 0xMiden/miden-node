use crate::generated::errors::SubmitProvenTransactionError;

impl SubmitProvenTransactionError {
    pub fn api_code(&self) -> u8 {
        *self as u8
    }

    pub fn tonic_code(&self) -> tonic::Code {
        match self {
            SubmitProvenTransactionError::InternalError => tonic::Code::Internal,
            _ => tonic::Code::InvalidArgument,
        }
    }

    pub fn is_internal(&self) -> bool {
        matches!(
            self,
            SubmitProvenTransactionError::InternalError | SubmitProvenTransactionError::Unspecified
        )
    }
}
