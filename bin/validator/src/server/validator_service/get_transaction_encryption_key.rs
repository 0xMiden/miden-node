use miden_node_proto::generated as grpc;
use miden_node_utils::tracing::miden_instrument;
use miden_tx::utils::serde::Serializable;

use super::ValidatorService;
use crate::COMPONENT;

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
        // Built entirely from state fixed at construction (selected by the in-memory chain tip), so
        // the endpoint stays available while a backup subscription holds the serve lock.
        let attested = self.effective_encryption_key();
        let validator_public_key = self.signer.public_key().to_bytes();
        let key_message = |scheme: u32, key_id: &[u8], public_key: &[u8], signature: &[u8]| {
            grpc::transaction::TransactionEncryptionKey {
                scheme: i32::try_from(scheme).expect("scheme identifier must fit in i32"),
                key_id: key_id.to_vec(),
                public_key: public_key.to_vec(),
                attestations: vec![grpc::transaction::ValidatorKeyAttestation {
                    validator_public_key: validator_public_key.clone(),
                    signature: signature.to_vec(),
                }],
            }
        };
        Ok(grpc::transaction::TransactionEncryptionKeyResponse {
            current_key: Some(key_message(
                attested.info.scheme,
                &attested.info.key_id,
                &attested.info.public_key,
                &attested.attestation.to_bytes(),
            )),
            next_key: attested.info.next_key.as_ref().map(|next| {
                let next_attestation = attested
                    .next_attestation
                    .as_ref()
                    .expect("a scheduled next key must carry its attestation");
                grpc::transaction::NextTransactionEncryptionKey {
                    key: Some(key_message(
                        next.scheme,
                        &next.key_id,
                        &next.public_key,
                        &next_attestation.to_bytes(),
                    )),
                    rotation_block_num: next.rotation_block_num,
                }
            }),
        })
    }
}
