use miden_node_db::sqlite::Database;
use miden_node_proto::generated as proto;
use miden_node_proto::generated::server::validator_admin_api;
use miden_protocol::utils::serde::Serializable;
use rand_core_06::OsRng;
use tonic::Status;

use crate::db::load_validated_private_transactions_page;
use crate::{GoldenOperatorKey, PrivateRecordError};

const DEFAULT_PAGE_SIZE: u32 = 50;
const MAX_PAGE_SIZE: u32 = 200;
const PAGE_TOKEN_BYTES: usize = size_of::<i64>();

/// Implements the private validator administration API.
pub(crate) struct ValidatorAdminService {
    operator_key: GoldenOperatorKey,
    database: Database,
}

impl ValidatorAdminService {
    /// Creates an admin service that owns this validator's Golden secret share.
    pub(crate) const fn new(operator_key: GoldenOperatorKey, database: Database) -> Self {
        Self { operator_key, database }
    }
}

#[tonic::async_trait]
impl validator_admin_api::ListValidatedPrivateTransactions for ValidatorAdminService {
    type Input = proto::validator_admin::ListValidatedPrivateTransactionsRequest;
    type Output = proto::validator_admin::ListValidatedPrivateTransactionsResponse;

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
        let page_size = match request.page_size {
            0 => DEFAULT_PAGE_SIZE,
            page_size if page_size <= MAX_PAGE_SIZE => page_size,
            page_size => {
                return Err(Status::invalid_argument(format!(
                    "page size {page_size} exceeds maximum {MAX_PAGE_SIZE}",
                )));
            },
        };
        let after_sequence = decode_page_token(&request.page_token)?;
        let page = self
            .database
            .read("list validated private transactions", move |tx| {
                load_validated_private_transactions_page(tx, page_size, after_sequence)
            })
            .await
            .map_err(|_error| Status::internal("failed to list validated private transactions"))?;

        let transactions = page
            .records
            .into_iter()
            .map(|record| proto::validator_admin::ValidatedPrivateTransaction {
                transaction_id: record.context().transaction_id().to_bytes(),
                final_ciphertext: record.encrypted_record().to_vec(),
                cipher_nonce: record.nonce().to_vec(),
                encrypted_record_key: record.encrypted_record_key().to_vec(),
                decryption_context: record.context().to_bytes(),
            })
            .collect();
        let next_page_token = page.next_cursor.map_or_else(Vec::new, encode_page_token);

        Ok(Self::Output { transactions, next_page_token })
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

fn decode_page_token(page_token: &[u8]) -> tonic::Result<i64> {
    if page_token.is_empty() {
        return Ok(0);
    }
    let token: [u8; PAGE_TOKEN_BYTES] = page_token
        .try_into()
        .map_err(|_| Status::invalid_argument("invalid page token"))?;
    let sequence = i64::from_be_bytes(token);
    if sequence <= 0 {
        return Err(Status::invalid_argument("invalid page token"));
    }
    Ok(sequence)
}

fn encode_page_token(sequence: i64) -> Vec<u8> {
    sequence.to_be_bytes().to_vec()
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
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};
    use golden_ehtdh1::wire::{from_wire_bytes, to_wire_bytes};
    use golden_ehtdh1::{Ciphertext, Combiner, DecryptionShare};
    use golden_halo2curves::golden_group::Secp256k1GoldenGroup;
    use miden_node_proto::generated::server::validator_api;
    use miden_node_utils::clap::GrpcOptionsInternal;
    use miden_node_utils::shutdown::CancellationToken;
    use miden_protocol::Word;
    use miden_protocol::account::auth::AuthScheme;
    use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey;
    use miden_protocol::transaction::{TransactionId, TransactionInputs};
    use miden_protocol::utils::serde::{Deserializable, Serializable};
    use miden_testing::{Auth, MockChainBuilder};
    use rand_chacha_03::ChaCha20Rng;
    use rand_chacha_03::rand_core::SeedableRng;
    use tokio::net::TcpListener;
    use tokio_stream::wrappers::TcpListenerStream;
    use tonic::Code;

    use super::*;
    use crate::db::insert_validated_private_transaction;
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
        target_record_for_transaction(operator_key, transaction_id, seed, plaintext)
    }

    fn target_record_for_transaction(
        operator_key: &GoldenOperatorKey,
        transaction_id: TransactionId,
        seed: u8,
        plaintext: &[u8],
    ) -> StoredPrivateRecord {
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

    fn transaction_inputs() -> TransactionInputs {
        let mut builder = MockChainBuilder::new();
        let account = builder
            .add_existing_wallet(Auth::BasicAuth {
                auth_scheme: AuthScheme::Falcon512Poseidon2,
            })
            .unwrap();
        builder.build().unwrap().get_transaction_inputs(&account, &[], &[]).unwrap()
    }

    async fn test_database() -> (tempfile::TempDir, Database) {
        let directory = tempfile::tempdir().unwrap();
        let database = crate::db::setup(directory.path().join("validator.sqlite3")).await.unwrap();
        (directory, database)
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
        service: &ValidatorAdminService,
        request: proto::validator_admin::IssueDecryptionShareRequest,
    ) -> tonic::Result<proto::validator_admin::IssueDecryptionShareResponse> {
        validator_admin_api::IssueDecryptionShare::full(service, tonic::Request::new(request)).await
    }

    async fn list(
        service: &ValidatorAdminService,
        request: proto::validator_admin::ListValidatedPrivateTransactionsRequest,
    ) -> tonic::Result<proto::validator_admin::ListValidatedPrivateTransactionsResponse> {
        validator_admin_api::ListValidatedPrivateTransactions::full(
            service,
            tonic::Request::new(request),
        )
        .await
    }

    #[tokio::test]
    async fn listed_record_drives_threshold_recovery() {
        let mut operator_keys = operator_keys();
        let target = operator_keys.pop().unwrap();
        let second = operator_keys.pop().unwrap();
        let public_key_set = target.public_key_set().clone();
        let setup_context = target.setup_context().clone();
        let (_directory, database) = test_database().await;
        let target_service = ValidatorAdminService::new(target, database.clone());
        let second_service = ValidatorAdminService::new(second, database.clone());
        let inputs = transaction_inputs();
        let transaction_id = TransactionId::from_raw(Word::from([8u32, 7, 6, 5]));
        let record = target_record_for_transaction(
            &operator_keys[0],
            transaction_id,
            10,
            &inputs.to_bytes(),
        );
        let stored_record = record.clone();
        database
            .write("store listed private transaction", move |tx| {
                insert_validated_private_transaction(tx, &stored_record)
            })
            .await
            .unwrap();

        let response = list(
            &target_service,
            proto::validator_admin::ListValidatedPrivateTransactionsRequest {
                page_size: 1,
                page_token: Vec::new(),
            },
        )
        .await
        .unwrap();
        let [listed] = response.transactions.as_slice() else {
            panic!("expected one listed transaction");
        };
        assert_eq!(listed.transaction_id, transaction_id.to_bytes());
        assert_eq!(listed.final_ciphertext, record.encrypted_record());
        assert_eq!(listed.cipher_nonce, record.nonce());
        assert_eq!(listed.encrypted_record_key, record.encrypted_record_key());
        assert_eq!(listed.decryption_context, record.context().to_bytes());

        let request = proto::validator_admin::IssueDecryptionShareRequest {
            ciphertext: listed.encrypted_record_key.clone(),
            decryption_context: listed.decryption_context.clone(),
        };
        let share_bytes = [
            issue(&target_service, request.clone()).await.unwrap().decryption_share,
            issue(&second_service, request).await.unwrap().decryption_share,
        ];
        let ciphertext: Ciphertext<Secp256k1GoldenGroup> =
            from_wire_bytes(&listed.encrypted_record_key).unwrap();
        let shares = share_bytes
            .iter()
            .map(|share| from_wire_bytes::<DecryptionShare<Secp256k1GoldenGroup>>(share).unwrap())
            .collect::<Vec<_>>();
        let content_key = Combiner::new(public_key_set, setup_context)
            .unwrap()
            .combine_exact_with_associated_data(
                &ciphertext,
                &listed.decryption_context,
                &listed.decryption_context,
                &shares,
            )
            .unwrap();
        let nonce: [u8; 24] = listed.cipher_nonce.as_slice().try_into().unwrap();
        let plaintext = XChaCha20Poly1305::new_from_slice(&content_key)
            .unwrap()
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &listed.final_ciphertext,
                    aad: &listed.decryption_context,
                },
            )
            .unwrap();

        assert_eq!(TransactionInputs::read_from_bytes(&plaintext).unwrap(), inputs);
    }

    #[tokio::test]
    async fn list_paginates_records_in_insertion_order() {
        let mut keys = operator_keys();
        let (_directory, database) = test_database().await;
        let transaction_ids = [
            TransactionId::from_raw(Word::from([9u32, 0, 0, 0])),
            TransactionId::from_raw(Word::from([1u32, 0, 0, 0])),
            TransactionId::from_raw(Word::from([5u32, 0, 0, 0])),
        ];
        for (seed, transaction_id) in [11u8, 12, 13].into_iter().zip(transaction_ids) {
            let record = target_record_for_transaction(&keys[0], transaction_id, seed, b"record");
            database
                .write("store paginated private transaction", move |tx| {
                    insert_validated_private_transaction(tx, &record)
                })
                .await
                .unwrap();
        }
        let service = ValidatorAdminService::new(keys.remove(0), database);

        let first = list(
            &service,
            proto::validator_admin::ListValidatedPrivateTransactionsRequest {
                page_size: 2,
                page_token: Vec::new(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            first
                .transactions
                .iter()
                .map(|transaction| transaction.transaction_id.as_slice())
                .collect::<Vec<_>>(),
            transaction_ids[..2].iter().map(TransactionId::as_bytes).collect::<Vec<_>>(),
        );
        assert!(!first.next_page_token.is_empty());

        let second = list(
            &service,
            proto::validator_admin::ListValidatedPrivateTransactionsRequest {
                page_size: 2,
                page_token: first.next_page_token,
            },
        )
        .await
        .unwrap();
        assert_eq!(second.transactions.len(), 1);
        assert_eq!(second.transactions[0].transaction_id, transaction_ids[2].to_bytes());
        assert!(second.next_page_token.is_empty());
    }

    #[tokio::test]
    async fn list_applies_page_limits_and_validates_token() {
        let mut keys = operator_keys();
        let (_directory, database) = test_database().await;
        let records = (0..=MAX_PAGE_SIZE)
            .map(|index| {
                let transaction_id = TransactionId::from_raw(Word::from([index + 1, 0, 0, 0]));
                target_record_for_transaction(&keys[0], transaction_id, 14, b"record")
            })
            .collect::<Vec<_>>();
        database
            .write("store records for page limits", move |tx| {
                for record in records {
                    insert_validated_private_transaction(tx, &record)?;
                }
                Ok::<_, miden_node_db::DatabaseError>(())
            })
            .await
            .unwrap();
        let service = ValidatorAdminService::new(keys.remove(0), database);

        let default_page = list(
            &service,
            proto::validator_admin::ListValidatedPrivateTransactionsRequest {
                page_size: 0,
                page_token: Vec::new(),
            },
        )
        .await
        .unwrap();
        assert_eq!(default_page.transactions.len(), DEFAULT_PAGE_SIZE as usize);
        assert!(!default_page.next_page_token.is_empty());

        let maximum_page = list(
            &service,
            proto::validator_admin::ListValidatedPrivateTransactionsRequest {
                page_size: MAX_PAGE_SIZE,
                page_token: Vec::new(),
            },
        )
        .await
        .unwrap();
        assert_eq!(maximum_page.transactions.len(), MAX_PAGE_SIZE as usize);
        assert!(!maximum_page.next_page_token.is_empty());

        let oversized = list(
            &service,
            proto::validator_admin::ListValidatedPrivateTransactionsRequest {
                page_size: MAX_PAGE_SIZE + 1,
                page_token: Vec::new(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(oversized.code(), Code::InvalidArgument);

        for page_token in [vec![0], 0i64.to_be_bytes().to_vec()] {
            let invalid = list(
                &service,
                proto::validator_admin::ListValidatedPrivateTransactionsRequest {
                    page_size: 1,
                    page_token,
                },
            )
            .await
            .unwrap_err();
            assert_eq!(invalid.code(), Code::InvalidArgument);
        }
    }

    #[tokio::test]
    async fn two_validators_issue_shares_for_third_validator_ciphertext() {
        let mut operator_keys = operator_keys();
        let third = operator_keys.pop().unwrap();
        let second = operator_keys.pop().unwrap();
        let first = operator_keys.pop().unwrap();
        let (_directory, database) = test_database().await;
        let first_service = ValidatorAdminService::new(first, database.clone());
        let second_service = ValidatorAdminService::new(second, database);
        let plaintext = b"private transaction inputs";
        let record = target_record(&third, 1, plaintext);
        let request = PrivateRecordShareRequest::for_record(&record);
        let rpc_request = rpc_request(&record);

        let shares = [
            issue(&first_service, rpc_request.clone()).await.unwrap().decryption_share,
            issue(&second_service, rpc_request).await.unwrap().decryption_share,
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
        let (_directory, database) = test_database().await;
        let first_service = ValidatorAdminService::new(first, database.clone());
        let second_service = ValidatorAdminService::new(second, database);
        let first_record = target_record(&third, 2, b"same plaintext");
        let second_record = target_record(&third, 3, b"same plaintext");
        assert_eq!(first_record.context(), second_record.context());
        assert_ne!(first_record.encrypted_record_key(), second_record.encrypted_record_key(),);

        let shares = [
            issue(&first_service, rpc_request(&first_record))
                .await
                .unwrap()
                .decryption_share,
            issue(&second_service, rpc_request(&second_record))
                .await
                .unwrap()
                .decryption_share,
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
        let (_directory, database) = test_database().await;
        let record = target_record(&keys[0], 4, b"record");
        let context = record.context().to_bytes();

        let malformed = proto::validator_admin::IssueDecryptionShareRequest {
            ciphertext: vec![0],
            decryption_context: context.clone(),
        };
        assert_eq!(
            issue(&ValidatorAdminService::new(keys.remove(0), database.clone()), malformed,)
                .await
                .unwrap_err()
                .code(),
            Code::InvalidArgument,
        );

        let mut non_canonical = record.encrypted_record_key().to_vec();
        non_canonical.push(0);
        let non_canonical = proto::validator_admin::IssueDecryptionShareRequest {
            ciphertext: non_canonical,
            decryption_context: context.clone(),
        };
        assert_eq!(
            issue(&ValidatorAdminService::new(keys.remove(0), database.clone()), non_canonical,)
                .await
                .unwrap_err()
                .code(),
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
            issue(&ValidatorAdminService::new(keys.remove(0), database.clone()), wrong_size,)
                .await
                .unwrap_err()
                .code(),
            Code::InvalidArgument,
        );

        let wrong_context = proto::validator_admin::IssueDecryptionShareRequest {
            ciphertext: record.encrypted_record_key().to_vec(),
            decryption_context: b"wrong context".to_vec(),
        };
        assert_eq!(
            issue(&ValidatorAdminService::new(operator_keys().remove(0), database), wrong_context,)
                .await
                .unwrap_err()
                .code(),
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
        let (_directory, database) = test_database().await;
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
            database,
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
        assert!(
            admin_client
                .list_validated_private_transactions(
                    proto::validator_admin::ListValidatedPrivateTransactionsRequest {
                        page_size: 1,
                        page_token: Vec::new(),
                    },
                )
                .await
                .unwrap()
                .into_inner()
                .transactions
                .is_empty(),
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
        assert_eq!(
            admin_on_public
                .list_validated_private_transactions(
                    proto::validator_admin::ListValidatedPrivateTransactionsRequest {
                        page_size: 1,
                        page_token: Vec::new(),
                    },
                )
                .await
                .unwrap_err()
                .code(),
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
