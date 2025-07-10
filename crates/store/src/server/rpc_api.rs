use std::collections::BTreeSet;

use miden_node_proto::{
    convert,
    domain::account::{AccountInfo, AccountProofRequest},
    generated::{
        account::{self as account_proto, AccountSummary},
        blockchain as blockchain_proto, note as note_proto,
        store::{
            self as store_proto, BlockHeaderByNumberRequest, BlockHeaderByNumberResponse,
            CheckNullifiersResponse, rpc_server,
        },
        transaction::TransactionSummary,
    },
    try_convert,
};
use miden_objects::{
    account::AccountId, block::BlockNumber, crypto::hash::rpo::RpoDigest, note::NoteId,
    utils::Serializable,
};
use tonic::{Request, Response, Status};
use tracing::{debug, info, instrument};

use crate::{
    COMPONENT,
    server::api::{
        StoreApi, internal_error, read_account_id, read_account_ids, validate_nullifiers,
    },
};

// CLIENT ENDPOINTS
// ================================================================================================

#[tonic::async_trait]
impl rpc_server::Rpc for StoreApi {
    /// Returns block header for the specified block number.
    ///
    /// If the block number is not provided, block header for the latest block is returned.
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.get_block_header_by_number",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn get_block_header_by_number(
        &self,
        request: Request<BlockHeaderByNumberRequest>,
    ) -> Result<Response<BlockHeaderByNumberResponse>, Status> {
        self.get_block_header_by_number_inner(request).await
    }

    /// Returns info on whether the specified nullifiers have been consumed.
    ///
    /// This endpoint also returns Merkle authentication path for each requested nullifier which can
    /// be verified against the latest root of the nullifier database.
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.check_nullifiers",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn check_nullifiers(
        &self,
        request: Request<store_proto::Nullifiers>,
    ) -> Result<Response<CheckNullifiersResponse>, Status> {
        // Validate the nullifiers and convert them to Digest values. Stop on first error.
        let request = request.into_inner();

        let nullifiers = validate_nullifiers(&request.nullifiers)?;

        // Query the state for the request's nullifiers
        let proofs = self.state.check_nullifiers(&nullifiers).await;

        Ok(Response::new(CheckNullifiersResponse { proofs: convert(proofs) }))
    }

    /// Returns nullifiers that match the specified prefixes and have been consumed.
    ///
    /// Currently the only supported prefix length is 16 bits.
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.check_nullifiers_by_prefix",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn check_nullifiers_by_prefix(
        &self,
        request: Request<store_proto::CheckNullifiersByPrefixRequest>,
    ) -> Result<Response<store_proto::CheckNullifiersByPrefixResponse>, Status> {
        let request = request.into_inner();

        if request.prefix_len != 16 {
            return Err(Status::invalid_argument("Only 16-bit prefixes are supported"));
        }

        let nullifiers = self
            .state
            .check_nullifiers_by_prefix(
                request.prefix_len,
                request.nullifiers,
                BlockNumber::from(request.block_num),
            )
            .await?
            .into_iter()
            .map(|nullifier_info| {
                store_proto::check_nullifiers_by_prefix_response::NullifierUpdate {
                    nullifier: Some(nullifier_info.nullifier.into()),
                    block_num: nullifier_info.block_num.as_u32(),
                }
            })
            .collect();

        Ok(Response::new(store_proto::CheckNullifiersByPrefixResponse { nullifiers }))
    }

    /// Returns info which can be used by the client to sync up to the latest state of the chain
    /// for the objects the client is interested in.
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.sync_state",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn sync_state(
        &self,
        request: Request<store_proto::SyncStateRequest>,
    ) -> Result<Response<store_proto::SyncStateResponse>, Status> {
        let request = request.into_inner();

        let account_ids: Vec<AccountId> = read_account_ids(&request.account_ids)?;

        let (state, delta) = self
            .state
            .sync_state(request.block_num.into(), account_ids, request.note_tags)
            .await
            .map_err(internal_error)?;

        let accounts = state
            .account_updates
            .into_iter()
            .map(|account_info| AccountSummary {
                account_id: Some(account_info.account_id.into()),
                account_commitment: Some(account_info.account_commitment.into()),
                block_num: account_info.block_num.as_u32(),
            })
            .collect();

        let transactions = state
            .transactions
            .into_iter()
            .map(|transaction_summary| TransactionSummary {
                account_id: Some(transaction_summary.account_id.into()),
                block_num: transaction_summary.block_num.as_u32(),
                transaction_id: Some(transaction_summary.transaction_id.into()),
            })
            .collect();

        let notes = state.notes.into_iter().map(Into::into).collect();

        Ok(Response::new(store_proto::SyncStateResponse {
            chain_tip: self.state.latest_block_num().await.as_u32(),
            block_header: Some(state.block_header.into()),
            mmr_delta: Some(delta.into()),
            accounts,
            transactions,
            notes,
        }))
    }

    /// Returns info which can be used by the client to sync note state.
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.sync_notes",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn sync_notes(
        &self,
        request: Request<store_proto::NoteTags>,
    ) -> Result<Response<store_proto::SyncNotesResponse>, Status> {
        let request = request.into_inner();

        let (state, mmr_proof) = self
            .state
            .sync_notes(request.block_num.into(), request.note_tags)
            .await
            .map_err(internal_error)?;

        let notes = state.notes.into_iter().map(Into::into).collect();

        Ok(Response::new(store_proto::SyncNotesResponse {
            chain_tip: self.state.latest_block_num().await.as_u32(),
            block_header: Some(state.block_header.into()),
            mmr_path: Some((&mmr_proof.merkle_path).into()),
            notes,
        }))
    }

    /// Returns a list of Note's for the specified NoteId's.
    ///
    /// If the list is empty or no Note matched the requested NoteId and empty list is returned.
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.get_notes_by_id",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn get_notes_by_id(
        &self,
        request: Request<note_proto::NoteIds>,
    ) -> Result<Response<note_proto::CommittedNotes>, Status> {
        info!(target: COMPONENT, ?request);

        let note_ids = request.into_inner().ids;

        let note_ids: Vec<RpoDigest> = try_convert(note_ids)
            .map_err(|err| Status::invalid_argument(format!("Invalid NoteId: {err}")))?;

        let note_ids: Vec<NoteId> = note_ids.into_iter().map(From::from).collect();

        let notes = self
            .state
            .get_notes_by_id(note_ids)
            .await?
            .into_iter()
            .map(Into::into)
            .collect();

        Ok(Response::new(note_proto::CommittedNotes { notes }))
    }

    /// Returns details for public (public) account by id.
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.get_account_details",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn get_account_details(
        &self,
        request: Request<account_proto::AccountId>,
    ) -> Result<Response<account_proto::AccountDetails>, Status> {
        let request = request.into_inner();
        let account_id = read_account_id(Some(request)).map_err(|err| *err)?;
        let account_info: AccountInfo = self.state.get_account_details(account_id).await?;

        // TODO: revisit this, previous implementation was just returning only the summary, but it
        // is weird since the details are not empty.
        Ok(Response::new((&account_info).into()))
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.get_block_by_number",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn get_block_by_number(
        &self,
        request: Request<blockchain_proto::BlockNumber>,
    ) -> Result<Response<blockchain_proto::MaybeBlock>, Status> {
        let request = request.into_inner();

        debug!(target: COMPONENT, ?request);

        let block = self.state.load_block(request.block_num.into()).await?;

        Ok(Response::new(blockchain_proto::MaybeBlock { block }))
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.get_account_proofs",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn get_account_proofs(
        &self,
        request: Request<store_proto::GetAccountProofsRequest>,
    ) -> Result<Response<store_proto::AccountProofs>, Status> {
        debug!(target: COMPONENT, ?request);
        let store_proto::GetAccountProofsRequest {
            account_requests,
            include_headers,
            code_commitments,
        } = request.into_inner();

        let include_headers = include_headers.unwrap_or_default();
        let request_code_commitments: BTreeSet<RpoDigest> = try_convert(code_commitments)
            .map_err(|err| Status::invalid_argument(format!("Invalid code commitment: {err}")))?;

        let account_requests: Vec<AccountProofRequest> =
            try_convert(account_requests).map_err(|err| {
                Status::invalid_argument(format!("Invalid account proofs request: {err}"))
            })?;

        let (block_num, infos) = self
            .state
            .get_account_proofs(account_requests, request_code_commitments, include_headers)
            .await?;

        Ok(Response::new(store_proto::AccountProofs {
            block_num: block_num.as_u32(),
            account_proofs: infos,
        }))
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.get_account_state_delta",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn get_account_state_delta(
        &self,
        request: Request<store_proto::GetAccountStateDeltaRequest>,
    ) -> Result<Response<store_proto::AccountStateDelta>, Status> {
        let request = request.into_inner();

        debug!(target: COMPONENT, ?request);

        let account_id = read_account_id(request.account_id).map_err(|err| *err)?;
        let delta = self
            .state
            .get_account_state_delta(
                account_id,
                request.from_block_num.into(),
                request.to_block_num.into(),
            )
            .await?
            .map(|delta| delta.to_bytes());

        Ok(Response::new(store_proto::AccountStateDelta { delta }))
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "store.rpc_server.status",
        skip_all,
        level = "debug",
        ret(level = "debug"),
        err
    )]
    async fn status(
        &self,
        _request: Request<()>,
    ) -> Result<Response<store_proto::StoreStatus>, Status> {
        Ok(Response::new(store_proto::StoreStatus {
            version: env!("CARGO_PKG_VERSION").to_string(),
            status: "connected".to_string(),
            chain_tip: self.state.latest_block_num().await.as_u32(),
        }))
    }
}
