use std::{collections::BTreeSet, sync::Arc};

use miden_node_proto::{
    errors::ConversionError,
    generated::{
        self, requests::GetBlockHeaderByNumberRequest, responses::GetBlockHeaderByNumberResponse,
    },
};
use miden_objects::{
    account::AccountId,
    block::BlockNumber,
    crypto::hash::rpo::RpoDigest,
    note::{NoteId, Nullifier},
};
use tonic::{Request, Response, Status};
use tracing::{info, instrument};

use crate::{COMPONENT, state::State};

// STORE API
// ================================================================================================

pub struct StoreApi {
    pub(super) state: Arc<State>,
}

impl StoreApi {
    /// Shared implementation for all `get_block_header_by_number` endpoints.
    pub async fn get_block_header_by_number_inner(
        &self,
        request: Request<GetBlockHeaderByNumberRequest>,
    ) -> Result<Response<GetBlockHeaderByNumberResponse>, Status> {
        info!(target: COMPONENT, ?request);
        let request = request.into_inner();

        let block_num = request.block_num.map(BlockNumber::from);
        let (block_header, mmr_proof) = self
            .state
            .get_block_header(block_num, request.include_mmr_proof.unwrap_or(false))
            .await
            .map_err(internal_error)?;

        Ok(Response::new(GetBlockHeaderByNumberResponse {
            block_header: block_header.map(Into::into),
            chain_length: mmr_proof.as_ref().map(|p| p.forest as u32),
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

pub fn read_account_id(
    id: Option<generated::account::AccountId>,
) -> Result<AccountId, Box<Status>> {
    id.ok_or(invalid_argument("missing account ID"))?
        .try_into()
        .map_err(|err| invalid_argument(format!("invalid account ID: {err}")).into())
}

#[allow(clippy::result_large_err)]
#[instrument(level = "debug", target = COMPONENT, skip_all, err)]
pub fn read_account_ids(
    account_ids: &[generated::account::AccountId],
) -> Result<Vec<AccountId>, Status> {
    account_ids
        .iter()
        .cloned()
        .map(AccountId::try_from)
        .collect::<Result<_, ConversionError>>()
        .map_err(|_| invalid_argument("Byte array is not a valid AccountId"))
}

#[allow(clippy::result_large_err)]
#[instrument(level = "debug", target = COMPONENT, skip_all, err)]
pub fn validate_nullifiers(
    nullifiers: &[generated::digest::Digest],
) -> Result<Vec<Nullifier>, Status> {
    nullifiers
        .iter()
        .copied()
        .map(TryInto::try_into)
        .collect::<Result<_, ConversionError>>()
        .map_err(|_| invalid_argument("Digest field is not in the modulus range"))
}

#[allow(clippy::result_large_err)]
#[instrument(level = "debug", target = COMPONENT, skip_all, err)]
pub fn validate_notes(notes: &[generated::digest::Digest]) -> Result<Vec<NoteId>, Status> {
    notes
        .iter()
        .map(|digest| Ok(RpoDigest::try_from(digest)?.into()))
        .collect::<Result<_, ConversionError>>()
        .map_err(|_| invalid_argument("Digest field is not in the modulus range"))
}

#[instrument(level = "debug",target = COMPONENT, skip_all)]
pub fn read_block_numbers(block_numbers: &[u32]) -> BTreeSet<BlockNumber> {
    block_numbers.iter().map(|raw_number| BlockNumber::from(*raw_number)).collect()
}
