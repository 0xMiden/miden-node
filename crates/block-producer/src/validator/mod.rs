use miden_node_proto::clients::{Builder, ValidatorClient};
use miden_node_proto::generated as proto;
use miden_objects::block::{BlockBody, BlockHeader, ProposedBlock};
use miden_objects::utils::{Deserializable, Serializable};
use thiserror::Error;
use tracing::{info, instrument};
use url::Url;

use crate::COMPONENT;

// VALIDATOR ERROR
// ================================================================================================

#[derive(Debug, Error)]
pub enum ValidatorError {
    #[error("gRPC transport error: {0}")]
    Transport(#[from] tonic::Status),
    #[error("Failed to convert header: {0}")]
    HeaderConversion(String),
    #[error("Failed to deserialize body: {0}")]
    BodyDeserialization(String),
}

// VALIDATOR RESPONSE
// ================================================================================================

#[derive(Debug, Clone)]
pub struct ValidateBlockResponse {
    pub header: BlockHeader,
    pub body: BlockBody,
}

// VALIDATOR CLIENT
// ================================================================================================

/// Interface to the validator's block-producer gRPC API.
///
/// Essentially just a thin wrapper around the generated gRPC client which improves type safety.
#[derive(Clone, Debug)]
pub struct BlockProducerValidatorClient {
    client: ValidatorClient,
}

impl BlockProducerValidatorClient {
    /// Creates a new validator client with a lazy connection.
    pub fn new(validator_url: Url) -> Self {
        info!(target: COMPONENT, validator_endpoint = %validator_url, "Initializing validator client");

        let validator = Builder::new(validator_url)
            .without_tls()
            .without_timeout()
            .without_metadata_version()
            .without_metadata_genesis()
            .with_otel_context_injection()
            .connect_lazy::<ValidatorClient>();

        Self { client: validator }
    }

    #[instrument(target = COMPONENT, name = "validator.client.validate_block", skip_all, err)]
    pub async fn validate_block(
        &self,
        proposed_block: ProposedBlock,
    ) -> Result<ValidateBlockResponse, ValidatorError> {
        // Send request and receive response.
        let message = proto::blockchain::ProposedBlock {
            proposed_block: proposed_block.to_bytes(),
        };
        let request = tonic::Request::new(message);
        let response = self.client.clone().validate_block(request).await?;
        let response = response.into_inner();

        // Extract header from response (should always be present).
        let header_proto = response.header.expect("validator always returns a header");
        let header = BlockHeader::try_from(header_proto)
            .map_err(|err| ValidatorError::HeaderConversion(err.to_string()))?;

        // Extract body from response (should always be present).
        let body_proto = response.body.expect("validator always returns a body");
        let body = BlockBody::read_from_bytes(&body_proto.block_body)
            .map_err(|err| ValidatorError::BodyDeserialization(err.to_string()))?;

        Ok(ValidateBlockResponse { header, body })
    }
}
