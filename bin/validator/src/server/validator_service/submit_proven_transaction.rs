use std::sync::atomic::Ordering;

use miden_node_proto::domain::encryption::transaction_inputs_associated_data;
use miden_node_proto::generated as grpc;
use miden_node_utils::ErrorReport;
use miden_node_utils::tracing::{miden_instrument, miden_span_record};
use miden_protocol::transaction::{ProvenTransaction, TransactionId, TransactionInputs};
use miden_tx::utils::serde::{Deserializable, Serializable};
use rand_core_06::OsRng;
use tonic::Status;

use super::ValidatorService;
use crate::db::{insert_validated_private_transaction, transaction_exists};
use crate::tx_validation::validate_transaction;
use crate::{COMPONENT, PrivateRecordContext, PrivateRecordId};

#[tonic::async_trait]
impl grpc::server::validator_api::SubmitProvenTransaction for ValidatorService {
    type Input = Input;
    type Output = ();

    #[miden_instrument(
        target = COMPONENT,
        name = "submit_proven_transaction",
        skip_all,
        err,
    )]
    async fn handle(
        &self,
        input: Self::Input,
        _metadata: &tonic::metadata::MetadataMap,
        _extensions: &tonic::codegen::http::Extensions,
    ) -> tonic::Result<Self::Output> {
        let Input { tx, sealed } = input;
        let tx_id = tx.id();
        miden_span_record!(
            transaction.id = %tx_id,
        );

        let inputs = self.unseal_transaction_inputs(&sealed, tx_id).await?;

        // Reject requests while a backup subscription is streaming.
        let _guard = self
            .serve_lock
            .try_read()
            .map_err(|_| Status::resource_exhausted("validator is busy streaming a backup"))?;

        let transaction_exists = self
            .db
            .read("transaction_exists", move |tx| transaction_exists(tx, tx_id))
            .await
            .map_err(|err| {
                Status::internal(err.as_report_context("Failed to query transaction"))
            })?;
        if transaction_exists {
            return Ok(());
        }

        let private_inputs = inputs.to_bytes();

        // Validate the transaction.
        validate_transaction(tx, inputs).await.map_err(|err| {
            Status::invalid_argument(err.as_report_context("Invalid transaction"))
        })?;

        // Re-encrypt the private inputs under a fresh content key.
        let record_id = PrivateRecordId::new(tx_id, &self.signer.public_key());
        let context = PrivateRecordContext::new(
            self.private_record_chain_id,
            self.private_record_sealer.key_epoch(),
            tx_id,
        );
        let private_record = self
            .private_record_sealer
            .seal(&mut OsRng, record_id, context, &private_inputs)
            .map_err(|err| {
                Status::internal(err.as_report_context("Failed to protect transaction inputs"))
            })?;

        // Store the validated transaction and private record atomically.
        let count = self
            .db
            .write("insert_validated_private_transaction", move |tx| {
                insert_validated_private_transaction(tx, &private_record)
            })
            .await
            .map_err(|err| {
                Status::internal(err.as_report_context("Failed to insert transaction"))
            })?;

        self.validated_transactions_count.fetch_add(count as u64, Ordering::Relaxed);
        Ok(())
    }

    fn decode(request: grpc::transaction::ProvenTransaction) -> tonic::Result<Self::Input> {
        let tx = ProvenTransaction::read_from_bytes(&request.transaction).map_err(|err| {
            Status::invalid_argument(err.as_report_context("Invalid proven transaction"))
        })?;
        let sealed = request.sealed_transaction_inputs.ok_or_else(|| {
            Status::invalid_argument(
                "Missing sealed transaction inputs: fetch the encryption key with \
                 GetTransactionEncryptionKey and seal the transaction inputs against it",
            )
        })?;
        if sealed.ciphertext.is_empty() {
            return Err(Status::invalid_argument("Empty sealed transaction inputs ciphertext"));
        }

        Ok(Self::Input { tx, sealed })
    }

    fn encode(output: Self::Output) -> tonic::Result<()> {
        Ok(output)
    }
}

pub struct Input {
    tx: ProvenTransaction,
    sealed: grpc::transaction::SealedTransactionInputs,
}

impl ValidatorService {
    /// Unseals transaction inputs submitted for `tx_id`.
    async fn unseal_transaction_inputs(
        &self,
        sealed: &grpc::transaction::SealedTransactionInputs,
        tx_id: TransactionId,
    ) -> tonic::Result<TransactionInputs> {
        // The key the inputs were sealed against is whichever one the client held, which during a
        // rotation may be the previous, current or next key. The provider owns that decision, so
        // the key id is passed through to it rather than compared against a single key here.
        let attested = self
            .attested_encryption_key_schedule()
            .await
            .map_err(|err| Status::failed_precondition(err.to_string()))?;
        let chain_tip = *self.committed_tip.borrow();
        let scheme = attested.scheme_of(&sealed.key_id).as_u32();

        let associated_data = transaction_inputs_associated_data(
            scheme,
            &sealed.key_id,
            self.genesis_commitment,
            tx_id,
        );
        let plaintext = self
            .decrypter
            .decrypt_transaction_inputs(
                &sealed.key_id,
                chain_tip,
                &sealed.ciphertext,
                &associated_data,
            )
            .await
            .map_err(|err| {
                use crate::TransactionInputDecryptionError as DecryptionError;

                // A rejected key id is actionable: the client can refetch the schedule and reseal.
                // Deliberately does not echo this validator's own key ids, because the RPC relays
                // this status verbatim to the submitting client. An authentication failure, by
                // contrast, collapses a wrong key, tampered ciphertext, mismatched associated data
                // and corrupt framing into one error, so it cannot be any more specific than "it
                // did not authenticate". `{:#}` renders the anyhow context chain, which
                // `ErrorReport` cannot because it is not a `std::error::Error`.
                match err {
                    DecryptionError::PrematureKey { .. }
                    | DecryptionError::ExpiredKey { .. }
                    | DecryptionError::UnknownKey { .. } => Status::failed_precondition(
                        "Transaction inputs were sealed against an encryption key that is not \
                         currently accepted: re-fetch the key with GetTransactionEncryptionKey and \
                         seal the inputs again",
                    ),
                    err => Status::invalid_argument(format!(
                        "Failed to unseal the transaction inputs: {err:#}"
                    )),
                }
            })?;

        TransactionInputs::read_from_bytes(&plaintext).map_err(|err| {
            Status::invalid_argument(err.as_report_context("Invalid transaction inputs"))
        })
    }
}
