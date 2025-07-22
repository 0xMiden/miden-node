use std::net::SocketAddr;

use miden_node_proto::{
    clients::{BlockProducer, BlockProducerApiClient, Builder, StoreRpc, StoreRpcClient},
    errors::ConversionError,
    generated::{
        requests::{
            CheckNullifiersByPrefixRequest, CheckNullifiersRequest, GetAccountDetailsRequest,
            GetAccountProofsRequest, GetAccountStateDeltaRequest, GetBlockByNumberRequest,
            GetBlockHeaderByNumberRequest, GetNotesByIdRequest, SubmitProvenTransactionRequest,
            SyncNoteRequest, SyncStateRequest,
        },
        responses::{
            BlockProducerStatusResponse, CheckNullifiersByPrefixResponse, CheckNullifiersResponse,
            GetAccountDetailsResponse, GetAccountProofsResponse, GetAccountStateDeltaResponse,
            GetBlockByNumberResponse, GetBlockHeaderByNumberResponse, GetNotesByIdResponse,
            RpcStatusResponse, StoreStatusResponse, SubmitProvenTransactionResponse,
            SyncNoteResponse, SyncStateResponse,
        },
        rpc::api_server,
    },
    try_convert,
};
use miden_node_utils::{
    ErrorReport,
    limiter::{
        QueryParamAccountIdLimit, QueryParamLimiter, QueryParamNoteIdLimit, QueryParamNoteTagLimit,
        QueryParamNullifierLimit,
    },
};
use miden_objects::{
    MAX_NUM_FOREIGN_ACCOUNTS, MIN_PROOF_SECURITY_LEVEL, Word,
    account::{AccountId, delta::AccountUpdateDetails},
    transaction::ProvenTransaction,
    utils::serde::Deserializable,
};
use miden_tx::TransactionVerifier;
use tonic::{Request, Response, Status};
use tracing::{debug, info, instrument};

use crate::COMPONENT;

// RPC SERVICE
// ================================================================================================

pub struct RpcService {
    store: StoreRpcClient,
    block_producer: Option<BlockProducerApiClient>,
}

impl RpcService {
    pub(super) fn new(
        store_address: SocketAddr,
        block_producer_address: Option<SocketAddr>,
    ) -> Self {
        let store = {
            let store_url = format!("http://{store_address}");
            // SAFETY: The store_url is always valid as it is created from a `SocketAddr`.
            let store = Builder::new().with_address(store_url).connect_lazy::<StoreRpc>().unwrap();
            info!(target: COMPONENT, store_endpoint = %store_address, "Store client initialized");
            store
        };

        let block_producer = block_producer_address.map(|block_producer_address| {
            let block_producer_url = format!("http://{block_producer_address}");
            // SAFETY: The block_producer_url is always valid as it is created from a `SocketAddr`.
            let block_producer = Builder::new()
                .with_address(block_producer_url)
                .connect_lazy::<BlockProducer>()
                .unwrap();
            info!(
                target: COMPONENT,
                block_producer_endpoint = %block_producer_address,
                "Block producer client initialized",
            );
            block_producer
        });

        Self { store, block_producer }
    }
}

#[tonic::async_trait]
impl api_server::Api for RpcService {
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "rpc.server.check_nullifiers",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn check_nullifiers(
        &self,
        request: Request<CheckNullifiersRequest>,
    ) -> Result<Response<CheckNullifiersResponse>, Status> {
        debug!(target: COMPONENT, request = ?request.get_ref());

        check::<QueryParamNullifierLimit>(request.get_ref().nullifiers.len())?;

        // validate all the nullifiers from the user request
        for nullifier in &request.get_ref().nullifiers {
            let _: Word = nullifier
                .try_into()
                .or(Err(Status::invalid_argument("Word field is not in the modulus range")))?;
        }

        self.store.clone().check_nullifiers(request).await
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "rpc.server.check_nullifiers_by_prefix",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn check_nullifiers_by_prefix(
        &self,
        request: Request<CheckNullifiersByPrefixRequest>,
    ) -> Result<Response<CheckNullifiersByPrefixResponse>, Status> {
        debug!(target: COMPONENT, request = ?request.get_ref());

        check::<QueryParamNullifierLimit>(request.get_ref().nullifiers.len())?;

        self.store.clone().check_nullifiers_by_prefix(request).await
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "rpc.server.get_block_header_by_number",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn get_block_header_by_number(
        &self,
        request: Request<GetBlockHeaderByNumberRequest>,
    ) -> Result<Response<GetBlockHeaderByNumberResponse>, Status> {
        info!(target: COMPONENT, request = ?request.get_ref());

        self.store.clone().get_block_header_by_number(request).await
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "rpc.server.sync_state",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn sync_state(
        &self,
        request: Request<SyncStateRequest>,
    ) -> Result<Response<SyncStateResponse>, Status> {
        debug!(target: COMPONENT, request = ?request.get_ref());

        check::<QueryParamAccountIdLimit>(request.get_ref().account_ids.len())?;
        check::<QueryParamNoteTagLimit>(request.get_ref().note_tags.len())?;

        self.store.clone().sync_state(request).await
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "rpc.server.sync_notes",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn sync_notes(
        &self,
        request: Request<SyncNoteRequest>,
    ) -> Result<Response<SyncNoteResponse>, Status> {
        debug!(target: COMPONENT, request = ?request.get_ref());

        check::<QueryParamNoteTagLimit>(request.get_ref().note_tags.len())?;

        self.store.clone().sync_notes(request).await
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "rpc.server.get_notes_by_id",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn get_notes_by_id(
        &self,
        request: Request<GetNotesByIdRequest>,
    ) -> Result<Response<GetNotesByIdResponse>, Status> {
        debug!(target: COMPONENT, request = ?request.get_ref());

        check::<QueryParamNoteIdLimit>(request.get_ref().note_ids.len())?;

        // Validation checking for correct NoteId's
        let note_ids = request.get_ref().note_ids.clone();

        let _: Vec<Word> = try_convert(note_ids).map_err(|err: ConversionError| {
            Status::invalid_argument(err.as_report_context("invalid NoteId"))
        })?;

        self.store.clone().get_notes_by_id(request).await
    }

    #[instrument(parent = None, target = COMPONENT, name = "rpc.server.submit_proven_transaction", skip_all, err)]
    async fn submit_proven_transaction(
        &self,
        request: Request<SubmitProvenTransactionRequest>,
    ) -> Result<Response<SubmitProvenTransactionResponse>, Status> {
        debug!(target: COMPONENT, request = ?request.get_ref());

        let Some(block_producer) = &self.block_producer else {
            return Err(Status::unavailable(
                "Transaction submission not available in read-only mode",
            ));
        };

        let request = request.into_inner();

        let tx = ProvenTransaction::read_from_bytes(&request.transaction).map_err(|err| {
            Status::invalid_argument(err.as_report_context("invalid transaction"))
        })?;

        // Only allow deployment transactions for new network accounts
        if tx.account_id().is_network()
            && !matches!(tx.account_update().details(), AccountUpdateDetails::New(_))
        {
            return Err(Status::invalid_argument(
                "Network transactions may not be submitted by users yet",
            ));
        }

        // Compare the account delta commitment of the ProvenTransaction with the actual delta
        let delta_commitment = tx.account_update().account_delta_commitment();

        // Verify that the delta commitment matches the actual delta
        if let AccountUpdateDetails::Delta(delta) = tx.account_update().details() {
            let computed_commitment = delta.to_commitment();

            if computed_commitment != delta_commitment {
                return Err(Status::invalid_argument(
                    "Account delta commitment does not match the actual account delta",
                ));
            }
        }

        let tx_verifier = TransactionVerifier::new(MIN_PROOF_SECURITY_LEVEL);

        tx_verifier.verify(&tx).map_err(|err| {
            Status::invalid_argument(format!(
                "Invalid proof for transaction {}: {}",
                tx.id(),
                err.as_report()
            ))
        })?;

        block_producer.clone().submit_proven_transaction(request).await
    }

    /// Returns details for public (public) account by id.
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "rpc.server.get_account_details",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn get_account_details(
        &self,
        request: Request<GetAccountDetailsRequest>,
    ) -> std::result::Result<Response<GetAccountDetailsResponse>, Status> {
        debug!(target: COMPONENT, request = ?request.get_ref());

        // Validating account using conversion:
        let _account_id: AccountId = request
            .get_ref()
            .account_id
            .clone()
            .ok_or(Status::invalid_argument("account_id is missing"))?
            .try_into()
            .map_err(|err| Status::invalid_argument(format!("Invalid account id: {err}")))?;

        self.store.clone().get_account_details(request).await
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "rpc.server.get_block_by_number",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn get_block_by_number(
        &self,
        request: Request<GetBlockByNumberRequest>,
    ) -> Result<Response<GetBlockByNumberResponse>, Status> {
        let request = request.into_inner();

        debug!(target: COMPONENT, ?request);

        self.store.clone().get_block_by_number(request).await
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "rpc.server.get_account_state_delta",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn get_account_state_delta(
        &self,
        request: Request<GetAccountStateDeltaRequest>,
    ) -> Result<Response<GetAccountStateDeltaResponse>, Status> {
        let request = request.into_inner();

        debug!(target: COMPONENT, ?request);

        self.store.clone().get_account_state_delta(request).await
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "rpc.server.get_account_proofs",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn get_account_proofs(
        &self,
        request: Request<GetAccountProofsRequest>,
    ) -> Result<Response<GetAccountProofsResponse>, Status> {
        let request = request.into_inner();

        debug!(target: COMPONENT, ?request);

        if request.account_requests.len() > MAX_NUM_FOREIGN_ACCOUNTS as usize {
            return Err(Status::invalid_argument(format!(
                "Too many accounts requested: {}, limit: {MAX_NUM_FOREIGN_ACCOUNTS}",
                request.account_requests.len()
            )));
        }

        if request.account_requests.len() < request.code_commitments.len() {
            return Err(Status::invalid_argument(
                "The number of code commitments should not exceed the number of requested accounts.",
            ));
        }

        self.store.clone().get_account_proofs(request).await
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "rpc.server.status",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn status(&self, request: Request<()>) -> Result<Response<RpcStatusResponse>, Status> {
        debug!(target: COMPONENT, request = ?request);

        let store_status =
            self.store.clone().status(Request::new(())).await.map(Response::into_inner).ok();
        let block_producer_status = if let Some(block_producer) = &self.block_producer {
            block_producer
                .clone()
                .status(Request::new(()))
                .await
                .map(Response::into_inner)
                .ok()
        } else {
            None
        };

        Ok(Response::new(RpcStatusResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
            store_status: store_status.or(Some(StoreStatusResponse {
                status: "unreachable".to_string(),
                chain_tip: 0,
                version: "-".to_string(),
            })),
            block_producer_status: block_producer_status.or(Some(BlockProducerStatusResponse {
                status: "unreachable".to_string(),
                version: "-".to_string(),
            })),
        }))
    }
}

// LIMIT HELPERS
// ================================================================================================

/// Formats an "Out of range" error
fn out_of_range_error<E: core::fmt::Display>(err: E) -> Status {
    Status::out_of_range(err.to_string())
}

/// Check, but don't repeat ourselves mapping the error
#[allow(clippy::result_large_err)]
fn check<Q: QueryParamLimiter>(n: usize) -> Result<(), Status> {
    <Q as QueryParamLimiter>::check(n).map_err(out_of_range_error)
}
