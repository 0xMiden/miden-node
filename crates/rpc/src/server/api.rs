//! RPC API server implementation.
//!
//! This module provides the main RPC service that handles all API requests.
//! It delegates to various backend services (store, block producer, validator)
//! and performs validation and processing of requests.

// IMPORTS
// ================================================================================================

use std::time::Duration;

use anyhow::Context;
use miden_node_proto::clients::{BlockProducerClient, Builder, StoreRpcClient, ValidatorClient};
use miden_node_proto::errors::ConversionError;
use miden_node_proto::generated::rpc::MempoolStats;
use miden_node_proto::generated::rpc::api_server::{self, Api};
use miden_node_proto::generated::{self as proto};
use miden_node_proto::try_convert;
use miden_node_utils::ErrorReport;
use miden_node_utils::limiter::{
    QueryParamAccountIdLimit, QueryParamLimiter, QueryParamNoteIdLimit, QueryParamNoteTagLimit,
    QueryParamNullifierLimit,
};
use miden_objects::account::AccountId;
use miden_objects::batch::ProvenBatch;
use miden_objects::block::{BlockHeader, BlockNumber};
use miden_objects::transaction::ProvenTransaction;
use miden_objects::utils::serde::{Deserializable, Serializable};
use miden_objects::{MIN_PROOF_SECURITY_LEVEL, Word};
use miden_tx::TransactionVerifier;
use tonic::{IntoRequest, Request, Response, Status};
use tracing::{debug, info, instrument, warn};
use url::Url;

use crate::COMPONENT;
use crate::server::transaction_helpers::{
    rebuild_batch_without_decorators, rebuild_transaction_without_decorators,
    validate_batch_network_account_restrictions, validate_network_account_restriction,
};

// RPC SERVICE
// ================================================================================================

/// Main RPC service that handles all API requests.
///
/// The service maintains connections to:
/// - Store: For querying blockchain state
/// - Block Producer: For submitting transactions (optional, for read-only mode)
/// - Validator: For validating transactions
pub struct RpcService {
    store: StoreRpcClient,
    block_producer: Option<BlockProducerClient>,
    validator: ValidatorClient,
    genesis_commitment: Option<Word>,
}

impl RpcService {
    /// Creates a new RPC service with connections to backend services.
    ///
    /// All connections are established lazily and will be created when first used.
    pub(super) fn new(store_url: Url, block_producer_url: Option<Url>, validator_url: Url) -> Self {
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

        let validator = {
            info!(
                target: COMPONENT,
                validator_endpoint = %validator_url,
                "Initializing validator client",
            );
            Builder::new(validator_url)
                .without_tls()
                .without_timeout()
                .without_metadata_version()
                .without_metadata_genesis()
                .with_otel_context_injection()
                .connect_lazy::<ValidatorClient>()
        };

        Self {
            store,
            block_producer,
            validator,
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
    /// Uses exponential backoff with a base delay of 500ms and maximum delay of 30s.
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

// API IMPLEMENTATION
// ================================================================================================

#[tonic::async_trait]
impl api_server::Api for RpcService {
    // NULLIFIER OPERATIONS
    // --------------------------------------------------------------------------------------------

    /// Checks whether the provided nullifiers have been consumed.
    ///
    /// Validates that all nullifiers are within the modulus range and checks
    /// their consumption status via the store.
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

        // Validate all the nullifiers from the user request
        for nullifier in &request.get_ref().nullifiers {
            let _: Word = nullifier
                .try_into()
                .or(Err(Status::invalid_argument("Word field is not in the modulus range")))?;
        }

        self.store.clone().check_nullifiers(request).await
    }

    /// Synchronizes nullifiers with the store.
    ///
    /// Used to check which nullifiers from the provided list have been consumed.
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

    // BLOCK OPERATIONS
    // --------------------------------------------------------------------------------------------

    /// Retrieves a block header by its block number.
    ///
    /// Optionally includes an MMR (Merkle Mountain Range) proof if requested.
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

    /// Retrieves a full block by its block number.
    ///
    /// Returns `None` if the block doesn't exist.
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

    // ACCOUNT OPERATIONS
    // --------------------------------------------------------------------------------------------

    /// Returns details for a public account by its ID.
    ///
    /// Validates the account ID format before querying the store.
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

        // Validate account ID using conversion
        let _account_id: AccountId = request
            .get_ref()
            .clone()
            .try_into()
            .map_err(|err| Status::invalid_argument(format!("Invalid account id: {err}")))?;

        self.store.clone().get_account_details(request).await
    }

    /// Retrieves an account proof for the specified account.
    ///
    /// The proof can be used to verify the account's state in the blockchain.
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

    /// Synchronizes account vault state.
    ///
    /// Used to fetch the current state of an account's vault.
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

    // NOTE OPERATIONS
    // --------------------------------------------------------------------------------------------

    /// Retrieves notes by their IDs.
    ///
    /// Validates that all note IDs are in the correct format before querying.
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

        // Validate all note IDs are in correct format
        let note_ids = request.get_ref().ids.clone();

        let _: Vec<Word> =
            try_convert(note_ids)
                .collect::<Result<_, _>>()
                .map_err(|err: ConversionError| {
                    Status::invalid_argument(err.as_report_context("invalid NoteId"))
                })?;

        self.store.clone().get_notes_by_id(request).await
    }

    /// Retrieves a note script by its root hash.
    ///
    /// The script root uniquely identifies a note script in the system.
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

    // SYNC OPERATIONS
    // --------------------------------------------------------------------------------------------

    /// Synchronizes state for accounts and notes.
    ///
    /// Fetches the latest state for the specified account IDs and note tags.
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

    /// Synchronizes storage maps for accounts.
    ///
    /// Used to fetch the latest storage map state for accounts.
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

    /// Synchronizes notes by their tags.
    ///
    /// Fetches all notes matching the specified tags.
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

    /// Synchronizes transactions.
    ///
    /// Fetches transactions based on the provided criteria.
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

    // TRANSACTION SUBMISSION
    // --------------------------------------------------------------------------------------------

    /// Submits a proven transaction to the network.
    ///
    /// This method:
    /// 1. Validates the transaction format and proof
    /// 2. Strips decorators from output notes
    /// 3. Validates network account restrictions
    /// 4. Optionally re-executes the transaction if inputs are provided
    /// 5. Submits to the block producer
    ///
    /// Returns an error if the service is in read-only mode (no block producer).
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "rpc.server.submit_proven_transaction",
        skip_all,
        err
    )]
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

        // Parse and validate the transaction
        let tx = ProvenTransaction::read_from_bytes(&request.transaction).map_err(|err| {
            Status::invalid_argument(err.as_report_context("invalid transaction"))
        })?;

        // Rebuild transaction with decorators stripped from output notes
        let rebuilt_tx =
            rebuild_transaction_without_decorators(&tx).map_err(Status::invalid_argument)?;

        let mut request = request;
        request.transaction = rebuilt_tx.to_bytes();

        // Validate network account restrictions
        validate_network_account_restriction(&tx).map_err(Status::invalid_argument)?;

        // Verify the transaction proof
        let tx_verifier = TransactionVerifier::new(MIN_PROOF_SECURITY_LEVEL);

        tx_verifier.verify(&tx).map_err(|err| {
            Status::invalid_argument(format!(
                "Invalid proof for transaction {}: {}",
                tx.id(),
                err.as_report()
            ))
        })?;

        // If transaction inputs are provided, re-execute the transaction to validate it
        if request.transaction_inputs.is_some() {
            // Re-execute the transaction via the Validator
            match self.validator.clone().submit_proven_transaction(request.clone()).await {
                Ok(_) => {
                    debug!(
                        target: COMPONENT,
                        tx_id = %tx.id().to_hex(),
                        "Transaction validation successful"
                    );
                },
                Err(e) => {
                    warn!(
                        target: COMPONENT,
                        tx_id = %tx.id().to_hex(),
                        error = %e,
                        "Transaction validation failed, but continuing with submission"
                    );
                },
            }
        }

        block_producer.clone().submit_proven_transaction(request).await
    }

    /// Submits a proven batch of transactions to the network.
    ///
    /// This method:
    /// 1. Validates the batch format
    /// 2. Strips decorators from output notes
    /// 3. Validates network account restrictions for all transactions
    /// 4. Submits to the block producer
    ///
    /// Returns an error if the service is in read-only mode (no block producer).
    #[instrument(
        parent = None,
        target = COMPONENT,
        name = "rpc.server.submit_proven_batch",
        skip_all,
        err
    )]
    async fn submit_proven_batch(
        &self,
        request: tonic::Request<proto::transaction::ProvenTransactionBatch>,
    ) -> Result<tonic::Response<proto::blockchain::BlockNumber>, Status> {
        let Some(block_producer) = &self.block_producer else {
            return Err(Status::unavailable("Batch submission not available in read-only mode"));
        };

        let mut request = request.into_inner();

        // Parse and validate the batch
        let batch = ProvenBatch::read_from_bytes(&request.encoded)
            .map_err(|err| Status::invalid_argument(err.as_report_context("invalid batch")))?;

        // Rebuild batch with decorators stripped from output notes
        let rebuilt_batch =
            rebuild_batch_without_decorators(&batch).map_err(Status::invalid_argument)?;

        request.encoded = rebuilt_batch.to_bytes();

        // Validate network account restrictions for all transactions in the batch
        validate_batch_network_account_restrictions(&batch).map_err(Status::invalid_argument)?;

        block_producer.clone().submit_proven_batch(request).await
    }

    // STATUS OPERATIONS
    // --------------------------------------------------------------------------------------------

    /// Returns the current status of the RPC service and its dependencies.
    ///
    /// Includes status information for:
    /// - The RPC service itself (version)
    /// - The store service
    /// - The block producer service (if available)
    /// - The genesis commitment
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
            block_producer: block_producer_status.or(Some(proto::rpc::BlockProducerStatus {
                status: "unreachable".to_string(),
                version: "-".to_string(),
                chain_tip: 0,
                mempool_stats: Some(MempoolStats::default()),
            })),
            genesis_commitment: self.genesis_commitment.map(Into::into),
        }))
    }
}

// HELPER FUNCTIONS
// ================================================================================================

/// Formats an "Out of range" error for limit violations.
fn out_of_range_error<E: core::fmt::Display>(err: E) -> Status {
    Status::out_of_range(err.to_string())
}

/// Checks if a value is within the allowed limit for a query parameter.
///
/// This is a generic helper that works with any type implementing `QueryParamLimiter`.
/// It maps limit violations to appropriate gRPC status errors.
#[allow(clippy::result_large_err)]
fn check<Q: QueryParamLimiter>(n: usize) -> Result<(), Status> {
    <Q as QueryParamLimiter>::check(n).map_err(out_of_range_error)
}
