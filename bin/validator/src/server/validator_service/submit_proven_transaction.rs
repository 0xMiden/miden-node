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
use crate::db::{insert_validated_private_transaction, transaction_storage_is_complete};
use crate::tx_validation::validate_transaction;
use crate::{COMPONENT, PrivateRecordContext, PrivateRecordId, PrivateRecordSealer};

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

        let storage_complete = self
            .db
            .read("transaction_storage_is_complete", move |tx| {
                transaction_storage_is_complete(tx, tx_id)
            })
            .await
            .map_err(|err| {
                Status::internal(err.as_report_context("Failed to query transaction"))
            })?;
        if storage_complete {
            return Ok(());
        }

        let private_inputs = inputs.to_bytes();

        // Validate the transaction.
        validate_transaction(tx, inputs).await.map_err(|err| {
            Status::invalid_argument(err.as_report_context("Invalid transaction"))
        })?;

        // Re-encrypt the private inputs under a fresh content key.
        let context = PrivateRecordContext::new(
            self.private_record_chain_id,
            self.storage_key.key_epoch(),
            PrivateRecordId::for_transaction_inputs(tx_id),
            tx_id,
        );
        let private_record = PrivateRecordSealer::from_operator_key(&self.storage_key)
            .seal(&mut OsRng, context, &private_inputs)
            .map_err(|err| {
                Status::internal(err.as_report_context("Failed to protect transaction inputs"))
            })?;

        // Store the validated transaction and private record atomically.
        let count = self
            .db
            .write("insert_validated_private_transaction", move |tx| {
                insert_validated_private_transaction(tx, tx_id, &private_record)
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
        // Checked ahead of the unseal purely to turn what would otherwise be an indistinguishable
        // authentication failure into an actionable one. The key identifier is public metadata, so
        // there is nothing to leak by comparing it. Deliberately does not echo this validator's own
        // key id: the RPC relays this status verbatim to the submitting client.
        if sealed.key_id != self.encryption_key_info.key_id {
            return Err(Status::failed_precondition(
                "Transaction inputs were sealed against an unknown encryption key: re-fetch the \
                 key with GetTransactionEncryptionKey and seal the inputs again",
            ));
        }

        let associated_data = transaction_inputs_associated_data(
            self.encryption_key_info.scheme.as_u32(),
            &self.encryption_key_info.key_id,
            self.genesis_commitment,
            tx_id,
        );
        let plaintext = self
            .decrypter
            .decrypt_transaction_inputs(&sealed.ciphertext, &associated_data)
            .await
            .map_err(|err| {
                // The underlying scheme collapses a wrong key, tampered ciphertext, mismatched
                // associated data and corrupt framing into one error, so this cannot be any more
                // specific than "it did not authenticate". `{:#}` renders the anyhow context chain,
                // which `ErrorReport` cannot because it is not a `std::error::Error`.
                Status::invalid_argument(format!(
                    "Failed to unseal the transaction inputs: {err:#}"
                ))
            })?;

        TransactionInputs::read_from_bytes(&plaintext).map_err(|err| {
            Status::invalid_argument(err.as_report_context("Invalid transaction inputs"))
        })
    }
}
