use miden_node_proto::generated as grpc;
use miden_node_utils::tracing::miden_instrument;
use miden_tx::utils::serde::Serializable;

use super::ValidatorService;
use crate::COMPONENT;
use crate::signers::TransactionEncryptionKeyInfo;

#[tonic::async_trait]
impl grpc::server::validator_api::GetTransactionEncryptionKey for ValidatorService {
    type Input = ();
    type Output = grpc::transaction::TransactionEncryptionKeyResponse;

    fn decode(request: ()) -> tonic::Result<Self::Input> {
        Ok(request)
    }

    fn encode(
        output: Self::Output,
    ) -> tonic::Result<grpc::transaction::TransactionEncryptionKeyResponse> {
        Ok(output)
    }

    #[miden_instrument(
        target = COMPONENT,
        name = "get_transaction_encryption_key",
        skip_all,
        err,
    )]
    async fn handle(
        &self,
        _input: Self::Input,
        _metadata: &tonic::metadata::MetadataMap,
        _extensions: &tonic::codegen::http::Extensions,
    ) -> tonic::Result<Self::Output> {
        // The schedule is cached in memory and re-attested lazily at most once per epoch, so this
        // endpoint remains independent of the backup serve lock.
        let attested = self
            .attested_encryption_key_schedule()
            .await
            .map_err(|err| tonic::Status::failed_precondition(err.to_string()))?;
        let validator_public_key = self.signer.public_key().to_bytes();

        let current_key = encode_key(&attested.schedule.current_key);
        let next_key = attested.schedule.next_key.as_ref().map(|next| {
            grpc::transaction::NextTransactionEncryptionKey {
                key: Some(encode_key(&next.key)),
                activation_block_num: next.activation_block_num.as_u32(),
            }
        });

        Ok(grpc::transaction::TransactionEncryptionKeyResponse {
            current_key: Some(current_key),
            next_key,
            current_key_activation_block_num: attested
                .schedule
                .current_key_activation_block_num
                .as_u32(),
            attestation_epoch: u32::from(attested.epoch),
            attestations: vec![grpc::transaction::ValidatorKeyAttestation {
                validator_public_key,
                signature: attested.attestation.to_bytes(),
            }],
        })
    }
}

/// Encodes one encryption key in wire format.
fn encode_key(key: &TransactionEncryptionKeyInfo) -> grpc::transaction::TransactionEncryptionKey {
    grpc::transaction::TransactionEncryptionKey {
        scheme: i32::try_from(key.scheme).expect("scheme identifier must fit in i32"),
        key_id: key.key_id.clone(),
        public_key: key.public_key.clone(),
    }
}
