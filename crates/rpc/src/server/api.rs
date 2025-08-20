use std::time::Duration;

use anyhow::Context;
use miden_node_proto::clients::{
    BlockProducer,
    BlockProducerClient,
    Builder,
    StoreRpc,
    StoreRpcClient,
};
use miden_node_proto::errors::ConversionError;
use miden_node_proto::generated::rpc::api_server::{self, Api};
use miden_node_proto::generated::{self as proto};
use miden_node_proto::try_convert;
use miden_node_utils::ErrorReport;
use miden_node_utils::limiter::{
    QueryParamAccountIdLimit,
    QueryParamLimiter,
    QueryParamNoteIdLimit,
    QueryParamNoteTagLimit,
    QueryParamNullifierLimit,
};
use miden_objects::account::AccountId;
use miden_objects::account::delta::AccountUpdateDetails;
use miden_objects::batch::ProvenBatch;
use miden_objects::block::{BlockHeader, BlockNumber};
use miden_objects::transaction::ProvenTransaction;
use miden_objects::utils::serde::Deserializable;
use miden_objects::{MAX_NUM_FOREIGN_ACCOUNTS, MIN_PROOF_SECURITY_LEVEL, Word};
use miden_tx::TransactionVerifier;
use tonic::{IntoRequest, Request, Response, Status};
use tracing::{debug, info, instrument};
use url::Url;

use crate::COMPONENT;

// RPC SERVICE
// ================================================================================================

pub struct RpcService {
    store: StoreRpcClient,
    block_producer: Option<BlockProducerClient>,
}

impl RpcService {
    pub(super) fn new(store_url: Url, block_producer_url: Option<Url>) -> Self {
        let store = {
            info!(target: COMPONENT, store_endpoint = %store_url, "Initializing store client");
            Builder::new(store_url)
                .expect("Failed to initialize store endpoint")
                .without_tls()
                .without_timeout()
                .without_metadata_version()
                .without_metadata_genesis()
                .connect_lazy::<StoreRpc>()
        };

        let block_producer = block_producer_url.map(|block_producer_url| {
            info!(
                target: COMPONENT,
                block_producer_endpoint = %block_producer_url,
                "Initializing block producer client",
            );
            Builder::new(block_producer_url)
                .expect("Failed to initialize block-producer endpoint")
                .without_tls()
                .without_timeout()
                .without_metadata_version()
                .without_metadata_genesis()
                .connect_lazy::<BlockProducer>()
        });

        Self { store, block_producer }
    }

    /// Fetches the genesis block header from the store.
    ///
    /// Automatically retries until the store connection becomes available.
    pub async fn get_genesis_header_with_retry(&self) -> anyhow::Result<BlockHeader> {
        let mut retry_counter = 0;
        loop {
            let result = self
                .get_block_header_by_number(
                    proto::shared::BlockHeaderByNumberRequest {
                        block_num: Some(BlockNumber::GENESIS.as_u32()),
                        include_mmr_proof: None,
                    }
                    .into_request(),
                )
                .await;

            match result {
                Ok(header) => {
                    let header = header
                        .into_inner()
                        .block_header
                        .context("response is missing the header")?;
                    let header =
                        BlockHeader::try_from(header).context("failed to parse response")?;

                    return Ok(header);
                },
                Err(err) if err.code() == tonic::Code::Unavailable => {
                    // exponential backoff with base 500ms and max 30s
                    let backoff = Duration::from_millis(500)
                        .saturating_mul(1 << retry_counter)
                        .min(Duration::from_secs(30));

                    tracing::warn!(
                        ?backoff,
                        %retry_counter,
                        %err,
                        "connection failed while subscribing to the mempool, retrying"
                    );

                    retry_counter += 1;
                    tokio::time::sleep(backoff).await;
                },
                Err(other) => return Err(other.into()),
            }
        }
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
        request: Request<proto::rpc_store::NullifierList>,
    ) -> Result<Response<proto::rpc_store::CheckNullifiersResponse>, Status> {
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
        request: Request<proto::rpc_store::CheckNullifiersByPrefixRequest>,
    ) -> Result<Response<proto::rpc_store::CheckNullifiersByPrefixResponse>, Status> {
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
        request: Request<proto::shared::BlockHeaderByNumberRequest>,
    ) -> Result<Response<proto::shared::BlockHeaderByNumberResponse>, Status> {
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
        request: Request<proto::rpc_store::SyncStateRequest>,
    ) -> Result<Response<proto::rpc_store::SyncStateResponse>, Status> {
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
        request: Request<proto::rpc_store::SyncNotesRequest>,
    ) -> Result<Response<proto::rpc_store::SyncNotesResponse>, Status> {
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
        request: Request<proto::note::NoteIdList>,
    ) -> Result<Response<proto::note::CommittedNoteList>, Status> {
        debug!(target: COMPONENT, request = ?request.get_ref());

        check::<QueryParamNoteIdLimit>(request.get_ref().ids.len())?;

        // Validation checking for correct NoteId's
        let note_ids = request.get_ref().ids.clone();

        let _: Vec<Word> =
            try_convert(note_ids)
                .collect::<Result<_, _>>()
                .map_err(|err: ConversionError| {
                    Status::invalid_argument(err.as_report_context("invalid NoteId"))
                })?;

        self.store.clone().get_notes_by_id(request).await
    }

    #[instrument(parent = None, target = COMPONENT, name = "rpc.server.submit_proven_transaction", skip_all, err)]
    async fn submit_proven_transaction(
        &self,
        request: Request<proto::transaction::ProvenTransaction>,
    ) -> Result<Response<proto::block_producer::SubmitProvenTransactionResponse>, Status> {
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
            && !tx.account_update().initial_state_commitment().is_empty()
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

    #[instrument(parent = None, target = COMPONENT, name = "rpc.server.submit_proven_batch", skip_all, err)]
    async fn submit_proven_batch(
        &self,
        request: tonic::Request<proto::transaction::ProvenTransactionBatch>,
    ) -> Result<tonic::Response<proto::block_producer::SubmitProvenBatchResponse>, Status> {
        let Some(block_producer) = &self.block_producer else {
            return Err(Status::unavailable("Batch submission not available in read-only mode"));
        };

        let request = request.into_inner();

        let batch = ProvenBatch::read_from_bytes(&request.encoded)
            .map_err(|err| Status::invalid_argument(err.as_report_context("invalid batch")))?;

        // Only allow deployment transactions for new network accounts
        for tx in batch.transactions().as_slice() {
            if tx.account_id().is_network() && !tx.initial_state_commitment().is_empty() {
                return Err(Status::invalid_argument(
                    "Network transactions may not be submitted by users yet",
                ));
            }
        }

        block_producer.clone().submit_proven_batch(request).await
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
        request: Request<proto::account::AccountId>,
    ) -> std::result::Result<Response<proto::account::AccountDetails>, Status> {
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
        request: Request<proto::blockchain::BlockNumber>,
    ) -> Result<Response<proto::blockchain::MaybeBlock>, Status> {
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
        request: Request<proto::rpc_store::AccountStateDeltaRequest>,
    ) -> Result<Response<proto::rpc_store::AccountStateDelta>, Status> {
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
        request: Request<proto::rpc_store::AccountProofsRequest>,
    ) -> Result<Response<proto::rpc_store::AccountProofs>, Status> {
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
    async fn status(
        &self,
        request: Request<()>,
    ) -> Result<Response<proto::rpc::RpcStatus>, Status> {
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

        Ok(Response::new(proto::rpc::RpcStatus {
            version: env!("CARGO_PKG_VERSION").to_string(),
            store: store_status.or(Some(proto::rpc_store::StoreStatus {
                status: "unreachable".to_string(),
                chain_tip: 0,
                version: "-".to_string(),
            })),
            block_producer: block_producer_status.or(Some(
                proto::block_producer::BlockProducerStatus {
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
