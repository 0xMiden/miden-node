use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use miden_node_proto::clients::{BlockProducerClient, Builder, StoreRpcClient};
use miden_node_proto::errors::ConversionError;
use miden_node_proto::generated::rpc::MempoolStats;
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
use miden_objects::batch::ProvenBatch;
use miden_objects::block::{BlockHeader, BlockNumber};
use miden_objects::note::{Note, NoteRecipient, NoteScript};
use miden_objects::transaction::{
    OutputNote,
    ProvenTransaction,
    ProvenTransactionBuilder,
    TransactionInputs,
};
use miden_objects::utils::serde::{Deserializable, Serializable};
use miden_objects::{MIN_PROOF_SECURITY_LEVEL, Word};
use miden_tx::TransactionVerifier;
use tonic::{IntoRequest, Request, Response, Status};
use tracing::{debug, info, instrument, warn};
use url::Url;

use crate::COMPONENT;
use crate::server::validator;

// RPC SERVICE
// ================================================================================================

pub struct RpcService {
    store: StoreRpcClient,
    block_producer: Option<BlockProducerClient>,
    genesis_commitment: Option<Word>,
}

impl RpcService {
    pub(super) fn new(store_url: Url, block_producer_url: Option<Url>) -> Self {
        let store = {
            info!(target: COMPONENT, store_endpoint = %store_url, "Initializing store client");
            Builder::new(store_url)
                .without_tls()
                .without_timeout()
                .without_metadata_version()
                .without_metadata_genesis()
                .with_otel_context_injection()
                .connect_lazy::<StoreRpcClient>()
        };

        let block_producer = block_producer_url.map(|block_producer_url| {
            info!(
                target: COMPONENT,
                block_producer_endpoint = %block_producer_url,
                "Initializing block producer client",
            );
            Builder::new(block_producer_url)
                .without_tls()
                .without_timeout()
                .without_metadata_version()
                .without_metadata_genesis()
                .with_otel_context_injection()
                .connect_lazy::<BlockProducerClient>()
        });

        Self {
            store,
            block_producer,
            genesis_commitment: None,
        }
    }

    /// Sets the genesis commitment, returning an error if it is already set.
    ///
    /// Required since `RpcService::new()` sets up the `store` which is used to fetch the
    /// `genesis_commitment`.
    pub fn set_genesis_commitment(&mut self, commitment: Word) -> anyhow::Result<()> {
        if self.genesis_commitment.is_some() {
            return Err(anyhow::anyhow!("genesis commitment already set"));
        }
        self.genesis_commitment = Some(commitment);
        Ok(())
    }

    /// Fetches the genesis block header from the store.
    ///
    /// Automatically retries until the store connection becomes available.
    pub async fn get_genesis_header_with_retry(&self) -> anyhow::Result<BlockHeader> {
        let mut retry_counter = 0;
        loop {
            let result = self
                .get_block_header_by_number(
                    proto::rpc::BlockHeaderByNumberRequest {
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
        request: Request<proto::rpc::NullifierList>,
    ) -> Result<Response<proto::rpc::CheckNullifiersResponse>, Status> {
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
        name = "rpc.server.sync_nullifiers",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn sync_nullifiers(
        &self,
        request: Request<proto::rpc::SyncNullifiersRequest>,
    ) -> Result<Response<proto::rpc::SyncNullifiersResponse>, Status> {
        debug!(target: COMPONENT, request = ?request.get_ref());

        check::<QueryParamNullifierLimit>(request.get_ref().nullifiers.len())?;

        self.store.clone().sync_nullifiers(request).await
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
        request: Request<proto::rpc::BlockHeaderByNumberRequest>,
    ) -> Result<Response<proto::rpc::BlockHeaderByNumberResponse>, Status> {
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
        request: Request<proto::rpc::SyncStateRequest>,
    ) -> Result<Response<proto::rpc::SyncStateResponse>, Status> {
        debug!(target: COMPONENT, request = ?request.get_ref());

        check::<QueryParamAccountIdLimit>(request.get_ref().account_ids.len())?;
        check::<QueryParamNoteTagLimit>(request.get_ref().note_tags.len())?;

        self.store.clone().sync_state(request).await
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "rpc.server.sync_storage_maps",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn sync_storage_maps(
        &self,
        request: Request<proto::rpc::SyncStorageMapsRequest>,
    ) -> Result<Response<proto::rpc::SyncStorageMapsResponse>, Status> {
        debug!(target: COMPONENT, request = ?request.get_ref());

        self.store.clone().sync_storage_maps(request).await
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
        request: Request<proto::rpc::SyncNotesRequest>,
    ) -> Result<Response<proto::rpc::SyncNotesResponse>, Status> {
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

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "rpc.server.sync_account_vault",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn sync_account_vault(
        &self,
        request: tonic::Request<proto::rpc::SyncAccountVaultRequest>,
    ) -> std::result::Result<tonic::Response<proto::rpc::SyncAccountVaultResponse>, tonic::Status>
    {
        debug!(target: COMPONENT, request = ?request.get_ref());

        self.store.clone().sync_account_vault(request).await
    }

    #[instrument(parent = None, target = COMPONENT, name = "rpc.server.submit_proven_transaction", skip_all, err)]
    async fn submit_proven_transaction(
        &self,
        request: Request<proto::transaction::ProvenTransaction>,
    ) -> Result<Response<proto::blockchain::BlockNumber>, Status> {
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

        // Rebuild a new ProvenTransaction with decorators removed from output notes
        let mut builder = ProvenTransactionBuilder::new(
            tx.account_id(),
            tx.account_update().initial_state_commitment(),
            tx.account_update().final_state_commitment(),
            tx.account_update().account_delta_commitment(),
            tx.ref_block_num(),
            tx.ref_block_commitment(),
            tx.fee(),
            tx.expiration_block_num(),
            tx.proof().clone(),
        )
        .account_update_details(tx.account_update().details().clone())
        .add_input_notes(tx.input_notes().iter().cloned());

        let stripped_outputs = tx.output_notes().iter().map(|note| match note {
            OutputNote::Full(note) => {
                let mut mast = note.script().mast().clone();
                Arc::make_mut(&mut mast).strip_decorators();
                let script = NoteScript::from_parts(mast, note.script().entrypoint());
                let recipient =
                    NoteRecipient::new(note.serial_num(), script, note.inputs().clone());
                let new_note = Note::new(note.assets().clone(), *note.metadata(), recipient);
                OutputNote::Full(new_note)
            },
            other => other.clone(),
        });
        builder = builder.add_output_notes(stripped_outputs);
        let rebuilt_tx = builder.build().map_err(|e| Status::invalid_argument(e.to_string()))?;
        let mut request = request;
        request.transaction = rebuilt_tx.to_bytes();

        // Only allow deployment transactions for new network accounts
        if tx.account_id().is_network()
            && !tx.account_update().initial_state_commitment().is_empty()
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

        // If transaction inputs are provided, re-execute the transaction to validate it.
        if let Some(tx_inputs_bytes) = &request.transaction_inputs {
            // Deserialize the transaction inputs.
            let tx_inputs = TransactionInputs::read_from_bytes(tx_inputs_bytes).map_err(|err| {
                Status::invalid_argument(err.as_report_context("Invalid transaction inputs"))
            })?;
            // Re-execute the transaction.
            match validator::re_execute_transaction(tx_inputs).await {
                Ok(_executed_tx) => {
                    debug!(
                        target = COMPONENT,
                        tx_id = %tx.id().to_hex(),
                        "Transaction re-execution successful"
                    );
                },
                Err(e) => {
                    warn!(
                        target = COMPONENT,
                        tx_id = %tx.id().to_hex(),
                        error = %e,
                        "Transaction re-execution failed, but continuing with submission"
                    );
                },
            }
        }

        block_producer.clone().submit_proven_transaction(request).await
    }

    #[instrument(parent = None, target = COMPONENT, name = "rpc.server.submit_proven_batch", skip_all, err)]
    async fn submit_proven_batch(
        &self,
        request: tonic::Request<proto::transaction::ProvenTransactionBatch>,
    ) -> Result<tonic::Response<proto::blockchain::BlockNumber>, Status> {
        let Some(block_producer) = &self.block_producer else {
            return Err(Status::unavailable("Batch submission not available in read-only mode"));
        };

        let mut request = request.into_inner();

        let batch = ProvenBatch::read_from_bytes(&request.encoded)
            .map_err(|err| Status::invalid_argument(err.as_report_context("invalid batch")))?;

        // Build a new batch with output notes' decorators removed
        let stripped_outputs: Vec<OutputNote> = batch
            .output_notes()
            .iter()
            .map(|note| match note {
                OutputNote::Full(note) => {
                    let mut mast = note.script().mast().clone();
                    Arc::make_mut(&mut mast).strip_decorators();
                    let script = NoteScript::from_parts(mast, note.script().entrypoint());
                    let recipient =
                        NoteRecipient::new(note.serial_num(), script, note.inputs().clone());
                    let new_note = Note::new(note.assets().clone(), *note.metadata(), recipient);
                    OutputNote::Full(new_note)
                },
                other => other.clone(),
            })
            .collect();

        let rebuilt_batch = ProvenBatch::new(
            batch.id(),
            batch.reference_block_commitment(),
            batch.reference_block_num(),
            batch.account_updates().clone(),
            batch.input_notes().clone(),
            stripped_outputs,
            batch.batch_expiration_block_num(),
            batch.transactions().clone(),
        )
        .map_err(|e| Status::invalid_argument(e.to_string()))?;

        request.encoded = rebuilt_batch.to_bytes();

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
        name = "rpc.server.get_account_proof",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn get_account_proof(
        &self,
        request: Request<proto::rpc::AccountProofRequest>,
    ) -> Result<Response<proto::rpc::AccountProofResponse>, Status> {
        let request = request.into_inner();

        debug!(target: COMPONENT, ?request);

        self.store.clone().get_account_proof(request).await
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
            store: store_status.or(Some(proto::rpc::StoreStatus {
                status: "unreachable".to_string(),
                chain_tip: 0,
                version: "-".to_string(),
            })),
            block_producer: block_producer_status.or(Some(
                proto::rpc::BlockProducerStatus {
                    status: "unreachable".to_string(),
                    version: "-".to_string(),
                    mempool_stats: Some(MempoolStats::default()),
                },
            )),
            genesis_commitment: self.genesis_commitment.map(Into::into),
        }))
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "rpc.server.get_note_script_by_root",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn get_note_script_by_root(
        &self,
        request: Request<proto::note::NoteRoot>,
    ) -> Result<Response<proto::rpc::MaybeNoteScript>, Status> {
        debug!(target: COMPONENT, request = ?request);

        self.store.clone().get_note_script_by_root(request).await
    }

    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "rpc.server.sync_transactions",
        skip_all,
        ret(level = "debug"),
        err
    )]
    async fn sync_transactions(
        &self,
        request: Request<proto::rpc::SyncTransactionsRequest>,
    ) -> Result<Response<proto::rpc::SyncTransactionsResponse>, Status> {
        debug!(target: COMPONENT, request = ?request);

        self.store.clone().sync_transactions(request).await
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
