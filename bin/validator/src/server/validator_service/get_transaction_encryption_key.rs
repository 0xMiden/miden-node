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
        // Built entirely from in-memory attested state, so the endpoint stays available while a
        // backup subscription holds the serve lock.
        let attested = self.attested_encryption_keys();
        let validator_public_key = self.signer.public_key().to_bytes();

        let current_key = encode_key(
            &attested.keys.current,
            &validator_public_key,
            &attested.current_attestation.to_bytes(),
        );
        let next_key = attested.keys.next.as_ref().map(|next| {
            let attestation = attested
                .next_attestation
                .as_ref()
                .expect("a next key is always attested together with the current key");
            grpc::transaction::NextTransactionEncryptionKey {
                key: Some(encode_key(&next.key, &validator_public_key, &attestation.to_bytes())),
                rotation_block_num: next.rotation_block_num,
            }
        });

        Ok(grpc::transaction::TransactionEncryptionKeyResponse {
            current_key: Some(current_key),
            next_key,
        })
    }
}

/// Encodes one attested encryption key in wire format.
fn encode_key(
    key: &TransactionEncryptionKeyInfo,
    validator_public_key: &[u8],
    signature: &[u8],
) -> grpc::transaction::TransactionEncryptionKey {
    grpc::transaction::TransactionEncryptionKey {
        scheme: i32::try_from(key.scheme).expect("scheme identifier must fit in i32"),
        key_id: key.key_id.clone(),
        public_key: key.public_key.clone(),
        attestations: vec![grpc::transaction::ValidatorKeyAttestation {
            validator_public_key: validator_public_key.to_vec(),
            signature: signature.to_vec(),
        }],
    }
}
