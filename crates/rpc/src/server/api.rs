use std::net::SocketAddr;

use miden_node_proto::{
    errors::ConversionError,
    generated::{
        account as account_proto, block_producer as block_producer_proto,
        block_producer::api_client as block_producer_client,
        blockchain as blockchain_proto, note as note_proto,
        rpc::{RpcStatus, api_server},
        store as store_proto,
        store::rpc_client as store_client,
        transaction as transaction_proto,
    },
    try_convert,
};
use miden_node_utils::{
    ErrorReport,
    limiter::{
        QueryParamAccountIdLimit, QueryParamLimiter, QueryParamNoteIdLimit, QueryParamNoteTagLimit,
        QueryParamNullifierLimit,
    },
    tracing::grpc::OtelInterceptor,
};
use miden_objects::{
    Digest, MAX_NUM_FOREIGN_ACCOUNTS, MIN_PROOF_SECURITY_LEVEL,
    account::{AccountId, delta::AccountUpdateDetails},
    crypto::hash::rpo::RpoDigest,
    transaction::ProvenTransaction,
    utils::serde::Deserializable,
};
use miden_tx::TransactionVerifier;
use tonic::{
    Request, Response, Status, service::interceptor::InterceptedService, transport::Channel,
};
use tracing::{debug, info, instrument};

use crate::COMPONENT;

// RPC SERVICE
// ================================================================================================

type StoreClient = store_client::RpcClient<InterceptedService<Channel, OtelInterceptor>>;
type BlockProducerClient =
    block_producer_client::ApiClient<InterceptedService<Channel, OtelInterceptor>>;

pub struct RpcService {
    store: StoreClient,
    block_producer: Option<BlockProducerClient>,
}

impl RpcService {
    pub(super) fn new(
        store_address: SocketAddr,
        block_producer_address: Option<SocketAddr>,
    ) -> Self {
        let store = {
            let store_url = format!("http://{store_address}");
            // SAFETY: The store_url is always valid as it is created from a `SocketAddr`.
            let channel = tonic::transport::Endpoint::try_from(store_url).unwrap().connect_lazy();
            let store = store_client::RpcClient::with_interceptor(channel, OtelInterceptor);
            info!(target: COMPONENT, store_endpoint = %store_address, "Store client initialized");
            store
        };

        let block_producer = block_producer_address.map(|block_producer_address| {
            let block_producer_url = format!("http://{block_producer_address}");
            // SAFETY: The block_producer_url is always valid as it is created from a `SocketAddr`.
            let channel =
                tonic::transport::Endpoint::try_from(block_producer_url).unwrap().connect_lazy();
            let block_producer =
                block_producer_client::ApiClient::with_interceptor(channel, OtelInterceptor);
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
        request: Request<store_proto::Nullifiers>,
    ) -> Result<Response<store_proto::CheckNullifiersResponse>, Status> {
        debug!(target: COMPONENT, request = ?request.get_ref());

        check::<QueryParamNullifierLimit>(request.get_ref().nullifiers.len())?;

        // validate all the nullifiers from the user request
        for nullifier in &request.get_ref().nullifiers {
            let _: Digest = nullifier
                .try_into()
                .or(Err(Status::invalid_argument("Digest field is not in the modulus range")))?;
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
        request: Request<store_proto::CheckNullifiersByPrefixRequest>,
    ) -> Result<Response<store_proto::CheckNullifiersByPrefixResponse>, Status> {
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
        request: Request<store_proto::BlockHeaderByNumberRequest>,
    ) -> Result<Response<store_proto::BlockHeaderByNumberResponse>, Status> {
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
        request: Request<store_proto::SyncStateRequest>,
    ) -> Result<Response<store_proto::SyncStateResponse>, Status> {
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
        request: Request<store_proto::SyncNotesRequest>,
    ) -> Result<Response<store_proto::SyncNotesResponse>, Status> {
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
        request: Request<note_proto::NoteIds>,
    ) -> Result<Response<note_proto::CommittedNotes>, Status> {
        debug!(target: COMPONENT, request = ?request.get_ref());

        check::<QueryParamNoteIdLimit>(request.get_ref().ids.len())?;

        // Validation checking for correct NoteId's
        let note_ids = request.get_ref().ids.clone();

        let _: Vec<RpoDigest> = try_convert(note_ids).map_err(|err: ConversionError| {
            Status::invalid_argument(err.as_report_context("invalid NoteId"))
        })?;

        self.store.clone().get_notes_by_id(request).await
    }

    #[instrument(parent = None, target = COMPONENT, name = "rpc.server.submit_proven_transaction", skip_all, err)]
    async fn submit_proven_transaction(
        &self,
        request: Request<transaction_proto::ProvenTransaction>,
    ) -> Result<Response<block_producer_proto::SubmitProvenTransactionResponse>, Status> {
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
        request: Request<account_proto::AccountId>,
    ) -> std::result::Result<Response<account_proto::AccountDetails>, Status> {
        debug!(target: COMPONENT, request = ?request.get_ref());

        // Validating account using conversion:
        let _account_id: AccountId = request
            .get_ref()
            .clone()
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
        request: Request<blockchain_proto::BlockNumber>,
    ) -> Result<Response<blockchain_proto::MaybeBlock>, Status> {
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
        request: Request<store_proto::GetAccountStateDeltaRequest>,
    ) -> Result<Response<store_proto::AccountStateDelta>, Status> {
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
        request: Request<store_proto::GetAccountProofsRequest>,
    ) -> Result<Response<store_proto::AccountProofs>, Status> {
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
    async fn status(&self, request: Request<()>) -> Result<Response<RpcStatus>, Status> {
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

        Ok(Response::new(RpcStatus {
            version: env!("CARGO_PKG_VERSION").to_string(),
            store: store_status.or(Some(store_proto::StoreStatus {
                status: "unreachable".to_string(),
                chain_tip: 0,
                version: "-".to_string(),
            })),
            block_producer: block_producer_status.or(Some(
                block_producer_proto::BlockProducerStatus {
                    status: "unreachable".to_string(),
                    version: "-".to_string(),
                },
            )),
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
