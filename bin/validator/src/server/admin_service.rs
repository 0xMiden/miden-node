use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use miden_node_db::sqlite::Database;
use miden_protocol::utils::serde::Serializable;
use rand_core_06::OsRng;
use serde::{Deserialize, Serialize};

use crate::db::load_validated_private_transactions;
use crate::{GoldenOperatorKey, PrivateRecordError, StoredPrivateRecord};

const LIST_TRANSACTIONS_PATH: &str = "/admin/transactions";
const ISSUE_SHARE_PATH: &str = "/admin/decryption-share";

#[derive(Clone)]
struct ValidatorAdminService {
    operator_key: Arc<GoldenOperatorKey>,
    database: Database,
}

impl ValidatorAdminService {
    fn new(operator_key: GoldenOperatorKey, database: Database) -> Self {
        Self {
            operator_key: Arc::new(operator_key),
            database,
        }
    }
}

pub(super) fn router(operator_key: GoldenOperatorKey, database: Database) -> Router {
    Router::new()
        .route(LIST_TRANSACTIONS_PATH, get(list_validated_private_transactions))
        .route(ISSUE_SHARE_PATH, post(issue_decryption_share))
        .with_state(ValidatorAdminService::new(operator_key, database))
}

#[derive(Debug, Deserialize, Serialize)]
struct ValidatedPrivateTransaction {
    transaction_id: String,
    final_ciphertext: String,
    cipher_nonce: String,
    encrypted_record_key: String,
    decryption_context: String,
}

impl From<StoredPrivateRecord> for ValidatedPrivateTransaction {
    fn from(record: StoredPrivateRecord) -> Self {
        Self {
            transaction_id: hex::encode(record.context().transaction_id().to_bytes()),
            final_ciphertext: hex::encode(record.encrypted_record()),
            cipher_nonce: hex::encode(record.nonce()),
            encrypted_record_key: hex::encode(record.encrypted_record_key()),
            decryption_context: hex::encode(record.context().to_bytes()),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct ListValidatedPrivateTransactionsResponse {
    transactions: Vec<ValidatedPrivateTransaction>,
}

async fn list_validated_private_transactions(
    State(service): State<ValidatorAdminService>,
) -> Result<Json<ListValidatedPrivateTransactionsResponse>, ApiError> {
    let records = service
        .database
        .read("list validated private transactions", load_validated_private_transactions)
        .await
        .map_err(|_error| ApiError::internal("failed to list validated private transactions"))?;

    Ok(Json(ListValidatedPrivateTransactionsResponse {
        transactions: records.into_iter().map(Into::into).collect(),
    }))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct IssueDecryptionShareRequest {
    ciphertext: String,
    decryption_context: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct IssueDecryptionShareResponse {
    decryption_share: String,
}

async fn issue_decryption_share(
    State(service): State<ValidatorAdminService>,
    Json(request): Json<IssueDecryptionShareRequest>,
) -> Result<Json<IssueDecryptionShareResponse>, ApiError> {
    let ciphertext = decode_hex("ciphertext", &request.ciphertext)?;
    let decryption_context = decode_hex("decryption_context", &request.decryption_context)?;
    let decryption_share = service
        .operator_key
        .issue_decryption_share(&mut OsRng, &ciphertext, &decryption_context)
        .map_err(|error| map_share_error(&error))?;

    Ok(Json(IssueDecryptionShareResponse {
        decryption_share: hex::encode(decryption_share),
    }))
}

fn decode_hex(field: &str, value: &str) -> Result<Vec<u8>, ApiError> {
    hex::decode(value).map_err(|_error| ApiError::bad_request(format!("{field} must be valid hex")))
}

fn map_share_error(error: &PrivateRecordError) -> ApiError {
    match error {
        PrivateRecordError::InvalidGoldenEncoding(_)
        | PrivateRecordError::InvalidEncryptedRecordKey
        | PrivateRecordError::DecryptionContextMismatch => ApiError::bad_request(error.to_string()),
        _ => ApiError::internal("failed to issue Golden decryption share"),
    }
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(ErrorResponse { error: self.message })).into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};
    use golden_ehtdh1::wire::{from_wire_bytes, to_wire_bytes};
    use golden_ehtdh1::{Ciphertext, Combiner, DecryptionShare};
    use golden_halo2curves::golden_group::Secp256k1GoldenGroup;
    use miden_protocol::Word;
    use miden_protocol::account::auth::AuthScheme;
    use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey;
    use miden_protocol::transaction::{TransactionId, TransactionInputs};
    use miden_protocol::utils::serde::{Deserializable, Serializable};
    use miden_testing::{Auth, MockChainBuilder};
    use rand_chacha_03::ChaCha20Rng;
    use rand_chacha_03::rand_core::SeedableRng;
    use tower::ServiceExt;

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

    fn share_request(record: &StoredPrivateRecord) -> IssueDecryptionShareRequest {
        IssueDecryptionShareRequest {
            ciphertext: hex::encode(record.encrypted_record_key()),
            decryption_context: hex::encode(record.context().to_bytes()),
        }
    }

    async fn issue(
        service: &ValidatorAdminService,
        request: IssueDecryptionShareRequest,
    ) -> Result<IssueDecryptionShareResponse, ApiError> {
        issue_decryption_share(State(service.clone()), Json(request))
            .await
            .map(|Json(response)| response)
    }

    #[tokio::test]
    async fn listed_record_drives_threshold_recovery() {
        let mut keys = operator_keys();
        let record_owner = keys.pop().unwrap();
        let second = keys.pop().unwrap();
        let first = keys.pop().unwrap();
        let public_key_set = record_owner.public_key_set().clone();
        let setup_context = record_owner.setup_context().clone();
        let (_directory, database) = test_database().await;
        let first_service = ValidatorAdminService::new(first, database.clone());
        let second_service = ValidatorAdminService::new(second, database.clone());
        let inputs = transaction_inputs();
        let transaction_id = TransactionId::from_raw(Word::from([8u32, 7, 6, 5]));
        let record = target_record(&record_owner, transaction_id, 10, &inputs.to_bytes());
        let stored_record = record.clone();
        database
            .write("store listed private transaction", move |tx| {
                insert_validated_private_transaction(tx, &stored_record)
            })
            .await
            .unwrap();

        let Json(response) =
            list_validated_private_transactions(State(first_service.clone())).await.unwrap();
        let [listed] = response.transactions.as_slice() else {
            panic!("expected one listed transaction");
        };
        assert_eq!(listed.transaction_id, hex::encode(transaction_id.to_bytes()));
        assert_eq!(listed.final_ciphertext, hex::encode(record.encrypted_record()));
        assert_eq!(listed.cipher_nonce, hex::encode(record.nonce()));
        assert_eq!(listed.encrypted_record_key, hex::encode(record.encrypted_record_key()));
        assert_eq!(listed.decryption_context, hex::encode(record.context().to_bytes()));

        let request = share_request(&record);
        let share_bytes = [
            issue(&first_service, request.clone()).await.unwrap().decryption_share,
            issue(&second_service, request).await.unwrap().decryption_share,
        ];
        let ciphertext: Ciphertext<Secp256k1GoldenGroup> =
            from_wire_bytes(record.encrypted_record_key()).unwrap();
        let shares = share_bytes
            .iter()
            .map(|share| {
                let bytes = hex::decode(share).unwrap();
                from_wire_bytes::<DecryptionShare<Secp256k1GoldenGroup>>(&bytes).unwrap()
            })
            .collect::<Vec<_>>();
        let context = record.context().to_bytes();
        let content_key = Combiner::new(public_key_set, setup_context)
            .unwrap()
            .combine_exact_with_associated_data(&ciphertext, &context, &context, &shares)
            .unwrap();
        let plaintext = XChaCha20Poly1305::new_from_slice(&content_key)
            .unwrap()
            .decrypt(
                &XNonce::from(*record.nonce()),
                Payload {
                    msg: record.encrypted_record(),
                    aad: &context,
                },
            )
            .unwrap();

        assert_eq!(TransactionInputs::read_from_bytes(&plaintext).unwrap(), inputs);
    }

    #[tokio::test]
    async fn list_uses_insertion_order() {
        let mut keys = operator_keys();
        let (_directory, database) = test_database().await;
        let transaction_ids = [
            TransactionId::from_raw(Word::from([9u32, 0, 0, 0])),
            TransactionId::from_raw(Word::from([1u32, 0, 0, 0])),
            TransactionId::from_raw(Word::from([5u32, 0, 0, 0])),
        ];
        for (seed, transaction_id) in [11u8, 12, 13].into_iter().zip(transaction_ids) {
            let record = target_record(&keys[0], transaction_id, seed, b"record");
            database
                .write("store private transaction", move |tx| {
                    insert_validated_private_transaction(tx, &record)
                })
                .await
                .unwrap();
        }

        let service = ValidatorAdminService::new(keys.remove(0), database);
        let Json(response) = list_validated_private_transactions(State(service)).await.unwrap();
        assert_eq!(
            response
                .transactions
                .iter()
                .map(|transaction| transaction.transaction_id.as_str())
                .collect::<Vec<_>>(),
            transaction_ids
                .iter()
                .map(|transaction_id| hex::encode(transaction_id.to_bytes()))
                .collect::<Vec<_>>(),
        );
    }

    #[tokio::test]
    async fn shares_for_different_ciphertexts_are_not_reusable() {
        let mut keys = operator_keys();
        let record_owner = keys.pop().unwrap();
        let second = ValidatorAdminService::new(keys.pop().unwrap(), test_database().await.1);
        let first = ValidatorAdminService::new(keys.pop().unwrap(), test_database().await.1);
        let transaction_id = TransactionId::from_raw(Word::from([1u32, 2, 3, 4]));
        let first_record = target_record(&record_owner, transaction_id, 2, b"same plaintext");
        let second_record = target_record(&record_owner, transaction_id, 3, b"same plaintext");
        assert_eq!(first_record.context(), second_record.context());
        assert_ne!(first_record.encrypted_record_key(), second_record.encrypted_record_key());

        let shares = [
            issue(&first, share_request(&first_record)).await.unwrap().decryption_share,
            issue(&second, share_request(&second_record)).await.unwrap().decryption_share,
        ]
        .map(|share| hex::decode(share).unwrap());
        let request = PrivateRecordShareRequest::for_record(&first_record);
        let result = PrivateRecordCombiner::from_operator_key(&record_owner).unwrap().open(
            &request,
            &first_record,
            &shares,
        );

        assert!(matches!(result, Err(PrivateRecordError::ShareCombination(_))));
    }

    #[tokio::test]
    async fn invalid_share_requests_return_bad_request() {
        let mut keys = operator_keys();
        let record = target_record(
            &keys[0],
            TransactionId::from_raw(Word::from([1u32, 2, 3, 4])),
            4,
            b"record",
        );
        let context = record.context().to_bytes();
        let (_directory, database) = test_database().await;

        let invalid_hex = IssueDecryptionShareRequest {
            ciphertext: "not hex".to_owned(),
            decryption_context: hex::encode(&context),
        };
        let error =
            issue(&ValidatorAdminService::new(keys.remove(0), database.clone()), invalid_hex)
                .await
                .unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);

        let mut short_rng = ChaCha20Rng::from_seed([5; 32]);
        let short_ciphertext = keys[0]
            .sealing_key()
            .seal_bytes_with_associated_data(&mut short_rng, &[0; 31], &context)
            .unwrap();
        let wrong_size = IssueDecryptionShareRequest {
            ciphertext: hex::encode(to_wire_bytes(&short_ciphertext)),
            decryption_context: hex::encode(context),
        };
        let error =
            issue(&ValidatorAdminService::new(keys.remove(0), database.clone()), wrong_size)
                .await
                .unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);

        let wrong_context = IssueDecryptionShareRequest {
            ciphertext: hex::encode(record.encrypted_record_key()),
            decryption_context: hex::encode(b"wrong context"),
        };
        let error =
            issue(&ValidatorAdminService::new(operator_keys().remove(0), database), wrong_context)
                .await
                .unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn router_exposes_only_the_json_admin_routes() {
        let (_directory, database) = test_database().await;
        let app = router(operator_keys().remove(0), database);

        let response = app
            .clone()
            .oneshot(Request::get(LIST_TRANSACTIONS_PATH).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get("content-type").unwrap(), "application/json",);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), br#"{"transactions":[]}"#);

        let response = app
            .clone()
            .oneshot(
                Request::post(ISSUE_SHARE_PATH)
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"ciphertext":"not hex","decryption_context":""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let response = app.oneshot(Request::get("/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
