use miden_node_proto::generated as proto;
use miden_node_proto::generated::server::validator_admin_api;
use rand_core_06::OsRng;
use tonic::Status;

use crate::{GoldenOperatorKey, PrivateRecordError};

/// Implements the private validator administration API.
pub(crate) struct ValidatorAdminService {
    operator_key: GoldenOperatorKey,
}

impl ValidatorAdminService {
    /// Creates an admin service that owns this validator's Golden secret share.
    pub(crate) const fn new(operator_key: GoldenOperatorKey) -> Self {
        Self { operator_key }
    }
}

#[tonic::async_trait]
impl validator_admin_api::IssueDecryptionShare for ValidatorAdminService {
    type Input = proto::validator_admin::IssueDecryptionShareRequest;
    type Output = proto::validator_admin::IssueDecryptionShareResponse;

    fn decode(request: Self::Input) -> tonic::Result<Self::Input> {
        Ok(request)
    }

    fn encode(output: Self::Output) -> tonic::Result<Self::Output> {
        Ok(output)
    }

    async fn handle(
        &self,
        request: Self::Input,
        _metadata: &tonic::metadata::MetadataMap,
        _extensions: &tonic::codegen::http::Extensions,
    ) -> tonic::Result<Self::Output> {
        let decryption_share = self
            .operator_key
            .issue_decryption_share(&mut OsRng, &request.ciphertext, &request.decryption_context)
            .map_err(|error| map_share_error(&error))?;
        Ok(Self::Output { decryption_share })
    }
}

fn map_share_error(error: &PrivateRecordError) -> Status {
    match error {
        PrivateRecordError::InvalidGoldenEncoding(_)
        | PrivateRecordError::InvalidEncryptedRecordKey
        | PrivateRecordError::DecryptionContextMismatch => {
            Status::invalid_argument(error.to_string())
        },
        _ => Status::internal("failed to issue Golden decryption share"),
    }
}

#[cfg(test)]
mod tests {
    use golden_ehtdh1::wire::to_wire_bytes;
    use miden_node_proto::generated::server::validator_api;
    use miden_node_utils::clap::GrpcOptionsInternal;
    use miden_node_utils::shutdown::CancellationToken;
    use miden_protocol::Word;
    use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey;
    use miden_protocol::transaction::TransactionId;
    use miden_protocol::utils::serde::Deserializable;
    use rand_chacha_03::ChaCha20Rng;
    use rand_chacha_03::rand_core::SeedableRng;
    use tokio::net::TcpListener;
    use tokio_stream::wrappers::TcpListenerStream;
    use tonic::Code;

    use super::*;
    use crate::storage_key::tests::operator_keys;
    use crate::{
        PrivateRecordChainId,
        PrivateRecordCombiner,
        PrivateRecordContext,
        PrivateRecordId,
        PrivateRecordSealer,
        PrivateRecordShareRequest,
        StoredPrivateRecord,
    };

    fn target_record(
        operator_key: &GoldenOperatorKey,
        seed: u8,
        plaintext: &[u8],
    ) -> StoredPrivateRecord {
        let transaction_id = TransactionId::from_raw(Word::from([1u32, 2, 3, 4]));
        let signer = SigningKey::read_from_bytes(&[9; 32]).unwrap();
        let record_id = PrivateRecordId::new(transaction_id, &signer.public_key());
        let context = PrivateRecordContext::new(
            PrivateRecordChainId::new([7; 32]),
            operator_key.key_epoch(),
            transaction_id,
        );
        PrivateRecordSealer::from_operator_key(operator_key)
            .seal(&mut ChaCha20Rng::from_seed([seed; 32]), record_id, context, plaintext)
            .unwrap()
    }

    fn rpc_request(
        record: &StoredPrivateRecord,
    ) -> proto::validator_admin::IssueDecryptionShareRequest {
        proto::validator_admin::IssueDecryptionShareRequest {
            ciphertext: record.encrypted_record_key().to_vec(),
            decryption_context: record.context().to_bytes(),
        }
    }

    async fn issue(
        operator_key: GoldenOperatorKey,
        request: proto::validator_admin::IssueDecryptionShareRequest,
    ) -> tonic::Result<proto::validator_admin::IssueDecryptionShareResponse> {
        let service = ValidatorAdminService::new(operator_key);
        validator_admin_api::IssueDecryptionShare::full(&service, tonic::Request::new(request))
            .await
    }

    #[tokio::test]
    async fn two_validators_issue_shares_for_third_validator_ciphertext() {
        let mut operator_keys = operator_keys();
        let third = operator_keys.pop().unwrap();
        let second = operator_keys.pop().unwrap();
        let first = operator_keys.pop().unwrap();
        let plaintext = b"private transaction inputs";
        let record = target_record(&third, 1, plaintext);
        let request = PrivateRecordShareRequest::for_record(&record);
        let rpc_request = rpc_request(&record);

        let shares = [
            issue(first, rpc_request.clone()).await.unwrap().decryption_share,
            issue(second, rpc_request).await.unwrap().decryption_share,
        ];

        let opened = PrivateRecordCombiner::from_operator_key(&third)
            .unwrap()
            .open(&request, &record, &shares)
            .unwrap();
        assert_eq!(opened.as_slice(), plaintext);
    }

    #[tokio::test]
    async fn shares_for_different_ciphertexts_are_not_reusable_with_the_same_context() {
        let mut operator_keys = operator_keys();
        let third = operator_keys.pop().unwrap();
        let second = operator_keys.pop().unwrap();
        let first = operator_keys.pop().unwrap();
        let first_record = target_record(&third, 2, b"same plaintext");
        let second_record = target_record(&third, 3, b"same plaintext");
        assert_eq!(first_record.context(), second_record.context());
        assert_ne!(first_record.encrypted_record_key(), second_record.encrypted_record_key(),);

        let shares = [
            issue(first, rpc_request(&first_record)).await.unwrap().decryption_share,
            issue(second, rpc_request(&second_record)).await.unwrap().decryption_share,
        ];
        let request = PrivateRecordShareRequest::for_record(&first_record);
        let result = PrivateRecordCombiner::from_operator_key(&third).unwrap().open(
            &request,
            &first_record,
            &shares,
        );

        assert!(matches!(result, Err(PrivateRecordError::ShareCombination(_))));
    }

    #[tokio::test]
    async fn malformed_ciphertext_wrong_payload_size_and_context_mismatch_are_rejected() {
        let mut keys = operator_keys();
        let record = target_record(&keys[0], 4, b"record");
        let context = record.context().to_bytes();

        let malformed = proto::validator_admin::IssueDecryptionShareRequest {
            ciphertext: vec![0],
            decryption_context: context.clone(),
        };
        assert_eq!(
            issue(keys.remove(0), malformed).await.unwrap_err().code(),
            Code::InvalidArgument,
        );

        let mut non_canonical = record.encrypted_record_key().to_vec();
        non_canonical.push(0);
        let non_canonical = proto::validator_admin::IssueDecryptionShareRequest {
            ciphertext: non_canonical,
            decryption_context: context.clone(),
        };
        assert_eq!(
            issue(keys.remove(0), non_canonical).await.unwrap_err().code(),
            Code::InvalidArgument,
        );

        let mut short_rng = ChaCha20Rng::from_seed([5; 32]);
        let short_ciphertext = keys[0]
            .sealing_key()
            .seal_bytes_with_associated_data(&mut short_rng, &[0; 31], &context)
            .unwrap();
        let wrong_size = proto::validator_admin::IssueDecryptionShareRequest {
            ciphertext: to_wire_bytes(&short_ciphertext),
            decryption_context: context.clone(),
        };
        assert_eq!(
            issue(keys.remove(0), wrong_size).await.unwrap_err().code(),
            Code::InvalidArgument,
        );

        let wrong_context = proto::validator_admin::IssueDecryptionShareRequest {
            ciphertext: record.encrypted_record_key().to_vec(),
            decryption_context: b"wrong context".to_vec(),
        };
        assert_eq!(
            issue(operator_keys().remove(0), wrong_context).await.unwrap_err().code(),
            Code::InvalidArgument,
        );
    }

    #[derive(Clone, Copy)]
    struct PublicValidatorStub;

    #[tonic::async_trait]
    impl validator_api::GetTransactionEncryptionKey for PublicValidatorStub {
        type Input = ();
        type Output = proto::transaction::TransactionEncryptionKey;

        fn decode(request: ()) -> tonic::Result<Self::Input> {
            Ok(request)
        }

        fn encode(output: Self::Output) -> tonic::Result<Self::Output> {
            Ok(output)
        }

        async fn handle(
            &self,
            _input: Self::Input,
            _metadata: &tonic::metadata::MetadataMap,
            _extensions: &tonic::codegen::http::Extensions,
        ) -> tonic::Result<Self::Output> {
            Err(Status::unimplemented("stub"))
        }
    }

    #[tonic::async_trait]
    impl validator_api::Status for PublicValidatorStub {
        type Input = ();
        type Output = proto::validator::ValidatorStatus;

        fn decode(request: ()) -> tonic::Result<Self::Input> {
            Ok(request)
        }

        fn encode(output: Self::Output) -> tonic::Result<Self::Output> {
            Ok(output)
        }

        async fn handle(
            &self,
            _input: Self::Input,
            _metadata: &tonic::metadata::MetadataMap,
            _extensions: &tonic::codegen::http::Extensions,
        ) -> tonic::Result<Self::Output> {
            Err(Status::unimplemented("stub"))
        }
    }

    #[tonic::async_trait]
    impl validator_api::SubmitProvenTransaction for PublicValidatorStub {
        type Input = ();
        type Output = ();

        fn decode(_request: proto::transaction::ProvenTransaction) -> tonic::Result<Self::Input> {
            Ok(())
        }

        fn encode(output: Self::Output) -> tonic::Result<Self::Output> {
            Ok(output)
        }

        async fn handle(
            &self,
            _input: Self::Input,
            _metadata: &tonic::metadata::MetadataMap,
            _extensions: &tonic::codegen::http::Extensions,
        ) -> tonic::Result<Self::Output> {
            Err(Status::unimplemented("stub"))
        }
    }

    #[tonic::async_trait]
    impl validator_api::SignBlock for PublicValidatorStub {
        type Input = ();
        type Output = proto::blockchain::SignBlockResponse;

        fn decode(_request: proto::blockchain::ProposedBlock) -> tonic::Result<Self::Input> {
            Ok(())
        }

        fn encode(output: Self::Output) -> tonic::Result<Self::Output> {
            Ok(output)
        }

        async fn handle(
            &self,
            _input: Self::Input,
            _metadata: &tonic::metadata::MetadataMap,
            _extensions: &tonic::codegen::http::Extensions,
        ) -> tonic::Result<Self::Output> {
            Err(Status::unimplemented("stub"))
        }
    }

    #[tonic::async_trait]
    impl validator_api::BlockSubscription for PublicValidatorStub {
        type Input = ();
        type Item = proto::validator::BlockSubscriptionResponse;
        type ItemStream = tokio_stream::Empty<tonic::Result<Self::Item>>;

        fn decode(
            _request: proto::validator::BlockSubscriptionRequest,
        ) -> tonic::Result<Self::Input> {
            Ok(())
        }

        fn encode(item: Self::Item) -> tonic::Result<Self::Item> {
            Ok(item)
        }

        async fn handle(
            &self,
            _input: Self::Input,
            _metadata: &tonic::metadata::MetadataMap,
            _extensions: &tonic::codegen::http::Extensions,
        ) -> tonic::Result<Self::ItemStream> {
            Err(Status::unimplemented("stub"))
        }
    }

    #[tokio::test]
    async fn admin_method_is_registered_only_on_admin_listener() {
        let mut operator_keys = operator_keys();
        let record = target_record(&operator_keys[0], 6, b"record");
        let request = rpc_request(&record);
        let admin_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let admin_address = admin_listener.local_addr().unwrap();
        let public_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let public_address = public_listener.local_addr().unwrap();
        let shutdown = CancellationToken::new();

        let admin_server = super::super::ValidatorAdminServer {
            address: admin_address,
            grpc_options: GrpcOptionsInternal::test(),
            operator_key: operator_keys.remove(0),
        };
        let admin_shutdown = shutdown.clone();
        let admin_task = tokio::spawn(async move {
            admin_server.serve_on(admin_listener, admin_shutdown).await.unwrap();
        });
        let public_shutdown = shutdown.clone();
        let public_task = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(validator_api::service(PublicValidatorStub))
                .serve_with_incoming_shutdown(
                    TcpListenerStream::new(public_listener),
                    public_shutdown.cancelled_owned(),
                )
                .await
                .unwrap();
        });

        let mut admin_client = proto::validator_admin::api_client::ApiClient::connect(format!(
            "http://{admin_address}",
        ))
        .await
        .unwrap();
        assert!(
            !admin_client
                .issue_decryption_share(request.clone())
                .await
                .unwrap()
                .into_inner()
                .decryption_share
                .is_empty()
        );

        let mut admin_on_public = proto::validator_admin::api_client::ApiClient::connect(format!(
            "http://{public_address}",
        ))
        .await
        .unwrap();
        assert_eq!(
            admin_on_public.issue_decryption_share(request).await.unwrap_err().code(),
            Code::Unimplemented,
        );

        let mut public_on_admin =
            proto::validator::api_client::ApiClient::connect(format!("http://{admin_address}"))
                .await
                .unwrap();
        assert_eq!(public_on_admin.status(()).await.unwrap_err().code(), Code::Unimplemented);

        shutdown.cancel();
        admin_task.await.unwrap();
        public_task.await.unwrap();
    }
}
