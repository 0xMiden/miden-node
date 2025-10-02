#[cfg(test)]
mod tests {
    use miden_node_proto::errors::block_producer::SubmitProvenTransactionGrpcError;
    use miden_node_proto::errors::store::GrpcError;
    use miden_objects::Word;
    use miden_objects::block::BlockNumber;
    use miden_objects::note::{NoteId, Nullifier};
    use miden_tx::utils::DeserializationError;

    use crate::errors::{AddTransactionError, VerifyTxError};

    #[rstest::rstest]
    #[test]
    #[case::inputs_already_consumed_error(
        AddTransactionError::VerificationFailed(
            VerifyTxError::InputNotesAlreadyConsumed(vec![Nullifier::dummy(1)])
        ),
        SubmitProvenTransactionGrpcError::InputNotesAlreadyConsumed,
        "verification failed",
    )]
    #[case::unauthenticated_notes_not_found_error(
        AddTransactionError::VerificationFailed(
            VerifyTxError::UnauthenticatedNotesNotFound(vec![NoteId::new(Word::default(), Word::default())])
        ),
        SubmitProvenTransactionGrpcError::UnauthenticatedNotesNotFound,
        "verification failed",
    )]
    #[case::stale_inputs_error(
        AddTransactionError::StaleInputs { input_block: BlockNumber::from(100), stale_limit: BlockNumber::from(150) },
        SubmitProvenTransactionGrpcError::Internal,
        "Internal error",
    )]
    #[case::expired_error(
        AddTransactionError::Expired { expired_at: BlockNumber::from(200), limit: BlockNumber::from(250) },
        SubmitProvenTransactionGrpcError::TransactionExpired,
        "expired",
    )]
    #[case::transaction_deserialization_failed_error(
        AddTransactionError::TransactionDeserializationFailed(DeserializationError::InvalidValue("test error".to_string())),
        SubmitProvenTransactionGrpcError::DeserializationFailed,
        "deserialization",
    )]
    fn submit_transaction_error_mapping(
        #[case] uut: AddTransactionError,
        #[case] expected: SubmitProvenTransactionGrpcError,
        #[case] message: &str,
    ) {
        let status = tonic::Status::from(uut);
        assert_eq!(status.code(), expected.tonic_code());
        assert!(status.message().contains(message));

        let api_code = match status.details() {
            &[single] => single,
            other => panic!(
                "Details should contain exactly one byte for the api error code, but got {other:?}"
            ),
        };
        assert_eq!(api_code, expected.api_code());
    }
}
