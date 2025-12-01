use miden_node_proto::clients::{Builder, ValidatorClient};
use miden_node_proto::errors::{ConversionError, MissingFieldHelper};
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
    #[error("response content error: {0}")]
    ResponseContent(#[from] ConversionError),
    #[error("Failed to convert header: {0}")]
    HeaderConversion(String),
    #[error("Failed to deserialize body: {0}")]
    BodyDeserialization(String),
}

// VALIDATOR CLIENT
// ================================================================================================

/// Interface to the validator's gRPC API.
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
    pub async fn sign_block(
        &self,
        proposed_block: ProposedBlock,
    ) -> Result<(BlockHeader, BlockBody), ValidatorError> {
        // Send request and receive response.
        let message = proto::blockchain::ProposedBlock {
            proposed_block: proposed_block.to_bytes(),
        };
        let request = tonic::Request::new(message);
        let response = self.client.clone().sign_block(request).await?;
        let signed_block = response.into_inner();

        // Extract header from response.
        let header_proto = signed_block
            .header
            .ok_or(miden_node_proto::generated::blockchain::BlockHeader::missing_field("header"))
            .map_err(ValidatorError::ResponseContent)?;
        let header = BlockHeader::try_from(header_proto)
            .map_err(|err| ValidatorError::HeaderConversion(err.to_string()))?;

        // Extract body from response.
        let body_proto = signed_block
            .body
            .ok_or(miden_node_proto::generated::blockchain::BlockBody::missing_field("body"))
            .map_err(ValidatorError::ResponseContent)?;
        let body = BlockBody::read_from_bytes(&body_proto.block_body)
            .map_err(|err| ValidatorError::BodyDeserialization(err.to_string()))?;

        Ok((header, body))
    }
}
