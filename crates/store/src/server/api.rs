use std::collections::BTreeSet;
use std::sync::Arc;

use miden_node_proto::errors::ConversionError;
use miden_node_proto::generated as proto;
use miden_node_utils::ErrorReport;
use miden_objects::Word;
use miden_objects::account::AccountId;
use miden_objects::block::BlockNumber;
use miden_objects::note::Nullifier;
use tonic::{Request, Response, Status};
use tracing::{info, instrument};

use crate::COMPONENT;
use crate::state::State;

// STORE API
// ================================================================================================

pub struct StoreApi {
    pub(super) state: Arc<State>,
}

impl StoreApi {
    /// Shared implementation for all `get_block_header_by_number` endpoints.
    pub async fn get_block_header_by_number_inner(
        &self,
        request: Request<proto::shared::BlockHeaderByNumberRequest>,
    ) -> Result<Response<proto::shared::BlockHeaderByNumberResponse>, Status> {
        info!(target: COMPONENT, ?request);
        let request = request.into_inner();

        let block_num = request.block_num.map(BlockNumber::from);
        let (block_header, mmr_proof) = self
            .state
            .get_block_header(block_num, request.include_mmr_proof.unwrap_or(false))
            .await?;

        Ok(Response::new(proto::shared::BlockHeaderByNumberResponse {
            block_header: block_header.map(Into::into),
            chain_length: mmr_proof.as_ref().map(|p| p.forest.num_leaves() as u32),
            mmr_path: mmr_proof.map(|p| Into::into(&p.merkle_path)),
        }))
    }
}

// UTILITIES
// ================================================================================================

/// Formats an "Internal error" error
pub fn internal_error<E: core::fmt::Display>(err: E) -> Status {
    Status::internal(err.to_string())
}

/// Formats an "Invalid argument" error
pub fn invalid_argument<E: core::fmt::Display>(err: E) -> Status {
    Status::invalid_argument(err.to_string())
}

/// Converts `ConversionError` to Status for nullifier validation
pub fn conversion_error_to_status(value: &ConversionError) -> Status {
    invalid_argument(value.as_report_context("Invalid nullifier format"))
}

/// Reads a block range from a request, returning a specific error type if the field is missing
pub fn read_block_range<E>(
    block_range: Option<proto::rpc_store::BlockRange>,
    entity: &'static str,
) -> Result<proto::rpc_store::BlockRange, E>
where
    E: From<ConversionError>,
{
    block_range.ok_or_else(|| {
        ConversionError::MissingFieldInProtobufRepresentation { entity, field_name: "block_range" }
            .into()
    })
}

/// Reads and converts a root field from a request to Word, returning a specific error type if
/// conversion fails
pub fn read_root<E>(
    root: Option<proto::primitives::Digest>,
    entity: &'static str,
) -> Result<Word, E>
where
    E: From<ConversionError>,
{
    root.ok_or_else(|| ConversionError::MissingFieldInProtobufRepresentation {
        entity,
        field_name: "root",
    })?
    .try_into()
    .map_err(Into::into)
}

/// Converts a collection of proto primitives to Words, returning a specific error type if
/// conversion fails
pub fn convert_digests_to_words<E, I>(digests: I) -> Result<Vec<Word>, E>
where
    E: From<ConversionError>,
    I: IntoIterator,
    I::Item: TryInto<Word, Error = ConversionError>,
{
    digests
        .into_iter()
        .map(TryInto::try_into)
        .collect::<Result<Vec<_>, ConversionError>>()
        .map_err(Into::into)
}

/// Reads account IDs from a request, returning a specific error type if conversion fails
pub fn read_account_ids<E>(account_ids: &[proto::account::AccountId]) -> Result<Vec<AccountId>, E>
where
    E: From<ConversionError>,
{
    account_ids
        .iter()
        .cloned()
        .map(AccountId::try_from)
        .collect::<Result<_, ConversionError>>()
        .map_err(Into::into)
}

pub fn read_account_id<E>(id: Option<proto::account::AccountId>) -> Result<AccountId, E>
where
    E: From<ConversionError>,
{
    id.ok_or_else(|| {
        ConversionError::deserialization_error(
            "AccountId",
            miden_objects::crypto::utils::DeserializationError::InvalidValue(
                "Missing account ID".to_string(),
            ),
        )
    })?
    .try_into()
    .map_err(Into::into)
}

#[allow(clippy::result_large_err)]
#[instrument(level = "debug", target = COMPONENT, skip_all, err)]
pub fn validate_nullifiers<E>(nullifiers: &[proto::primitives::Digest]) -> Result<Vec<Nullifier>, E>
where
    E: From<ConversionError> + std::fmt::Display,
{
    nullifiers
        .iter()
        .copied()
        .map(TryInto::try_into)
        .collect::<Result<_, ConversionError>>()
        .map_err(Into::into)
}

#[allow(clippy::result_large_err)]
#[instrument(level = "debug", target = COMPONENT, skip_all, err)]
pub fn validate_note_commitments(notes: &[proto::primitives::Digest]) -> Result<Vec<Word>, Status> {
    notes
        .iter()
        .map(|digest| Word::try_from(digest).map_err(|_| ()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid_argument("Digest field is not in the modulus range"))
}

#[instrument(level = "debug",target = COMPONENT, skip_all)]
pub fn read_block_numbers(block_numbers: &[u32]) -> BTreeSet<BlockNumber> {
    block_numbers.iter().map(|raw_number| BlockNumber::from(*raw_number)).collect()
}
