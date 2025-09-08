#[cfg(test)]
mod tests {
    use miden_objects::block::BlockNumber;

    use crate::errors::{AddTransactionError, SubmitTransactionErrorCode};

    // Helper function to parse error code from a tonic::Status
    fn parse_error_code(status: &tonic::Status) -> Option<u8> {
        let details_bytes = status.details();
        if details_bytes.len() == 1 {
            Some(details_bytes[0])
        } else {
            None
        }
    }

    #[test]
    fn test_stale_inputs_error() {
        let input_block = BlockNumber::from_epoch(100);
        let stale_limit = BlockNumber::from_epoch(150);

        let error = AddTransactionError::StaleInputs { input_block, stale_limit };

        let status: tonic::Status = error.into();

        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("stale"));

        let error_code = parse_error_code(&status).expect("Expected error code in details");
        assert_eq!(error_code, SubmitTransactionErrorCode::StaleInputs as u8);
    }

    #[test]
    fn test_expired_error() {
        let expired_at = BlockNumber::from_epoch(200);
        let limit = BlockNumber::from_epoch(250);

        let error = AddTransactionError::Expired { expired_at, limit };

        let status: tonic::Status = error.into();

        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("expired"));

        let error_code = parse_error_code(&status).expect("Expected error code in details");
        assert_eq!(error_code, SubmitTransactionErrorCode::Expired as u8);
    }

    #[test]
    fn test_deserialization_failed_error() {
        use miden_objects::utils::DeserializationError;

        let source_error = DeserializationError::InvalidValue("test error".to_string());
        let error = AddTransactionError::TransactionDeserializationFailed(source_error);

        let status: tonic::Status = error.into();

        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("deserialization"));

        let error_code = parse_error_code(&status).expect("Expected error code in details");
        assert_eq!(error_code, SubmitTransactionErrorCode::DeserializationFailed as u8);
    }

    #[test]
    fn test_verification_errors() {
        use miden_objects::Word;
        use miden_objects::note::{NoteId, Nullifier};

        use crate::errors::VerifyTxError;

        // Test InputNotesAlreadyConsumed
        let nullifiers = vec![Nullifier::dummy(1)];
        let verify_error = VerifyTxError::InputNotesAlreadyConsumed(nullifiers);
        let error = AddTransactionError::VerificationFailed(verify_error);
        let status: tonic::Status = error.into();

        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("verification failed"));

        let error_code = parse_error_code(&status).expect("Expected error code in details");
        assert_eq!(error_code, SubmitTransactionErrorCode::InputNotesAlreadyConsumed as u8);

        // Test UnauthenticatedNotesNotFound
        let note_ids = vec![NoteId::new(Word::default(), Word::default())];
        let verify_error = VerifyTxError::UnauthenticatedNotesNotFound(note_ids);
        let error = AddTransactionError::VerificationFailed(verify_error);
        let status: tonic::Status = error.into();

        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("verification failed"));

        let error_code = parse_error_code(&status).expect("Expected error code in details");
        assert_eq!(error_code, SubmitTransactionErrorCode::UnauthenticatedNotesNotFound as u8);
    }
}
