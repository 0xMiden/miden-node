use std::{collections::BTreeSet, net::SocketAddr, num::NonZeroUsize, sync::Arc, time::Duration};

use anyhow::Context;
use block_producer::BlockProducerClient;
use data_store::NtxBuilderDataStore;
use futures::TryFutureExt;
use miden_node_proto::{
    domain::{account::NetworkAccountError, note::NetworkNote},
    generated::ntx_builder::api_server,
};
use miden_node_utils::tracing::OpenTelemetrySpanExt;
use miden_objects::{
    AccountError, TransactionInputError,
    account::AccountId,
    assembly::DefaultSourceManager,
    block::BlockNumber,
    note::{Note, NoteId, NoteTag},
    transaction::{ExecutedTransaction, InputNote, InputNotes, TransactionArgs},
};
use miden_tx::{
    NoteAccountExecution, NoteConsumptionChecker, TransactionExecutor, TransactionExecutorError,
    TransactionProverError,
};
use prover::NtbTransactionProver;
use server::{NtxBuilderApi, PendingNotes};
use store::{StoreClient, StoreError};
use thiserror::Error;
use tokio::{
    net::TcpListener,
    runtime::Builder as RtBuilder,
    sync::Mutex,
    task::{JoinHandle, spawn_blocking},
    time,
};
use tokio_stream::wrappers::TcpListenerStream;
use tower_http::trace::TraceLayer;
use tracing::{Instrument, Span, debug, error, info, info_span, instrument, warn};
use url::Url;

use crate::COMPONENT;

mod block_producer;
mod data_store;
mod prover;
mod server;
mod store;

type SharedPendingNotes = Arc<Mutex<PendingNotes>>;

// NETWORK TRANSACTION REQUEST
// ================================================================================================

#[derive(Clone, Debug)]
struct NetworkTransactionRequest {
    pub account_id: AccountId,
    pub block_ref: BlockNumber,
    pub notes_to_execute: Vec<NetworkNote>,
}

impl NetworkTransactionRequest {
    fn new(account_id: AccountId, block_ref: BlockNumber, notes: Vec<NetworkNote>) -> Self {
        Self {
            account_id,
            block_ref,
            notes_to_execute: notes,
        }
    }
}

// NETWORK TRANSACTION BUILDER
// ================================================================================================

/// Network transaction builder component.
///
/// The network transaction builder is in in charge of building transactions that consume notes
/// against network accounts. These notes are identified and communicated by the block producer.
/// The service maintains a list of unconsumed notes and periodically executes and proves
/// transactions that consume them (reaching out to the store to retrieve state as necessary).
pub struct NetworkTransactionBuilder {
    /// The address for the network transaction builder gRPC server.
    pub ntx_builder_address: SocketAddr,
    /// Address of the store gRPC server.
    pub store_url: Url,
    /// Address of the block producer gRPC server.
    pub block_producer_address: SocketAddr,
    /// Address of the remote proving service. If `None`, transactions will be proven locally,
    /// which is undesirable due to the perofmrance impact.
    pub tx_prover_url: Option<Url>,
    /// Interval for checking pending notes and executing network transactions.
    pub ticker_interval: Duration,
    /// Capacity of the in-memory account cache for the executor's data store.
    pub account_cache_capacity: NonZeroUsize,
}

impl NetworkTransactionBuilder {
    /// Serves the transaction builder service.
    ///
    /// If for any reason the service errors, it gets restarted.
    pub async fn serve_resilient(&mut self) -> anyhow::Result<()> {
        loop {
            match self.serve_once().await {
                Ok(()) => warn!(target: COMPONENT, "builder stopped without error, restarting"),
                Err(e) => warn!(target: COMPONENT, error = %e, "builder crashed, restarting"),
            }

            // sleep before retrying to not spin the server
            time::sleep(Duration::from_secs(1)).await;
        }
    }

    #[instrument(parent = None, target = COMPONENT, name = "ntx_builder.serve_once", skip_all, err)]
    pub async fn serve_once(&self) -> anyhow::Result<()> {
        let store = StoreClient::new(&self.store_url);
        let unconsumed = store.get_unconsumed_network_notes().await?;
        let notes_queue = Arc::new(Mutex::new(PendingNotes::new(unconsumed)));

        let listener = TcpListener::bind(self.ntx_builder_address).await?;
        let server = tonic::transport::Server::builder()
            .accept_http1(true)
            .layer(TraceLayer::new_for_grpc())
            .add_service(api_server::ApiServer::new(NtxBuilderApi::new(notes_queue.clone())))
            .serve_with_incoming(TcpListenerStream::new(listener));
        tokio::pin!(server);

        let mut ticker = self.spawn_ticker(notes_queue.clone());

        loop {
            tokio::select! {
                // gRPC server ended, shut down ticker and bubble error up
                result = &mut server => {
                    ticker.abort();
                    return result.context("gRPC server stopped");
                }
                // ticker ended or panicked, respawn it; RPC server keeps running
                outcome = &mut ticker => {
                    match outcome {
                        Ok(Ok(())) => warn!(target: COMPONENT, "ticker stopped; respawning"),
                        Ok(Err(e)) => error!(target: COMPONENT, error=%e, "ticker errored; respawning"),
                        Err(join_err) => error!(target: COMPONENT, error=%join_err, "ticker panicked; respawning"),
                    }
                    ticker = self.spawn_ticker(notes_queue.clone());
                }
            }
        }
    }

    /// Spawns the ticker task and returns a handle to it.
    ///
    /// The ticker is in charge of periodically checking the network notes set and executing the
    /// next set of notes.
    fn spawn_ticker(&self, api_state: SharedPendingNotes) -> JoinHandle<anyhow::Result<()>> {
        let store_url = self.store_url.clone();
        let block_addr = self.block_producer_address;
        let prover_addr = self.tx_prover_url.clone();
        let ticker_interval = self.ticker_interval;

        spawn_blocking(move || {
            let rt = RtBuilder::new_current_thread()
                .enable_all()
                .build()
                .context("failed to build runtime")?;

            rt.block_on(async move {
                info!(target: COMPONENT, "Spawned NTB ticker (ticks every {} ms)", &ticker_interval.as_millis());
                let store = StoreClient::new(&store_url);
                let data_store = Arc::new(NtxBuilderDataStore::new(store).await?);
                let tx_executor = TransactionExecutor::new(data_store.clone(), None);
                let tx_prover = NtbTransactionProver::from(prover_addr);
                let block_prod = BlockProducerClient::new(block_addr);

                let mut interval = time::interval(ticker_interval);

                loop {
                    interval.tick().await;

                    let result = Self::build_network_tx(
                        &api_state,
                        &tx_executor,
                        &data_store,
                        &tx_prover,
                        &block_prod,
                    )
                    .await;

                    if let Err(e) = result {
                        error!(target: COMPONENT,err=%e, "Error preparing transaction");
                    }
                }
            })
        })
    }

    /// Performs all steps to submit a proven transaction to the block producer:
    ///
    /// - (preflight) Gets the next tag and set of notes to consume.
    ///   - With this, MMR peaks and the latest header is retrieved
    ///   - The executor account is retrieved from the cache or store.
    ///   - If the executor account is not found, the notes are **discarded** and note requeued.
    /// - Executes, proves and submits a network transaction.
    /// - After executing, updates the account cache with the new account state and any notes that
    ///   were note used are requeued
    ///
    /// A failure on the second stage will result in the transaction being rolled back.
    ///
    /// ## Telemetry
    ///
    /// - Creates a new root span which means each transaction gets its own complete trace.
    /// - Adds an `ntx.tag` attribute to the whole span to describe the account that will execute
    ///   the ntx.
    /// - Each stage has its own child span and are free to add further field data.
    /// - A failed step on the execution stage will emit an error event, and both its own span and
    ///   the root span will be marked as errors.
    ///
    /// # Errors
    ///
    /// - Returns an error only when the preflight stage errors. On the execution stage, errors are
    ///   logged and the transaction gets rolled back.
    async fn build_network_tx(
        api_state: &SharedPendingNotes,
        tx_executor: &TransactionExecutor,
        data_store: &Arc<NtxBuilderDataStore>,
        tx_prover: &NtbTransactionProver,
        block_prod: &BlockProducerClient,
    ) -> Result<(), NtxBuilderError> {
        // Preflight: Look for next account and blockchain data, and select notes
        let Some(tx_request) = Self::select_next_tx(api_state, data_store).await? else {
            debug!(target: COMPONENT, "No notes for existing network accounts found, returning.");
            return Ok(());
        };

        // Execution: Filter notes, execute, prove and submit tx
        let executed_tx = Self::filter_consumable_notes(data_store,tx_executor, &tx_request)
            .and_then(|filtered_tx_req| Self::execute_transaction(tx_executor, filtered_tx_req))
            .and_then(|executed_tx| Self::prove_and_submit_transaction(tx_prover, block_prod, executed_tx))
            .inspect_ok(|tx| {
                info!(target: COMPONENT, tx_id = %tx.id(), "Proved and submitted network transaction");
            })
            .inspect_err(|err| {
                warn!(target: COMPONENT, error = %err, "Error in transaction processing");
                Span::current().set_error(err);
            })
            .instrument(Span::current())
            .await;

        // If execution succeeded, requeue notes we did not use and update account cache
        if let Ok(tx) = executed_tx {
            let executed_ids: BTreeSet<NoteId> =
                tx.input_notes().iter().map(InputNote::id).collect();
            let failed_notes = tx_request
                .notes_to_execute
                .iter()
                .filter(|n| !executed_ids.contains(&n.id()))
                .cloned();

            api_state.lock().await.queue_unconsumed_notes(failed_notes);

            data_store
                .update_account(&tx)
                .map_err(NtxBuilderError::AccountCacheUpdateFailed)?;
        } else {
            // Otherwise, roll back
            Self::rollback_tx(tx_request, api_state, data_store).await;
        }

        Ok(())
    }

    /// Selects the next tag and set of notes to execute.
    /// If a tag is in queue, we attempt to retrieve the account and update the datastore's partial
    /// MMR.
    ///
    /// If this function errors, the notes are effectively discarded because [`Self::rollback_tx()`]
    /// is not called.
    async fn select_next_tx(
        api_state: &SharedPendingNotes,
        data_store: &Arc<NtxBuilderDataStore>,
    ) -> Result<Option<NetworkTransactionRequest>, NtxBuilderError> {
        let Some((tag, notes)) = api_state.lock().await.take_next_notes_by_tag() else {
            return Ok(None);
        };

        let span = info_span!("ntx_builder.select_next_batch");
        span.set_attribute("ntx.tag", tag.inner());

        let block_num = Self::prepare_blockchain_data(data_store).await?;
        let account_id = Self::get_account_for_ntx(data_store, tag).await?;

        match account_id {
            Some(id) => Ok(Some(NetworkTransactionRequest::new(id, block_num, notes))),
            // No network account found for note tag, discard (notes are not requeued)
            None => Ok(None),
        }
    }

    /// Updates the partial blockchain and latest header within the datastore.
    #[instrument(target = COMPONENT, name = "ntx_builder.prepare_blockchain_data", skip_all, err)]
    async fn prepare_blockchain_data(
        data_store: &Arc<NtxBuilderDataStore>,
    ) -> Result<BlockNumber, StoreError> {
        data_store.update_blockchain_data().await
    }

    /// Gets the account from the cache or from the store if it's not found in the cache.
    #[instrument(target = COMPONENT, name = "ntx_builder.get_account_for_batch", skip_all, err)]
    async fn get_account_for_ntx(
        data_store: &Arc<NtxBuilderDataStore>,
        tag: NoteTag,
    ) -> Result<Option<AccountId>, NtxBuilderError> {
        let account = data_store.get_cached_acc_or_fetch_by_tag(tag).await?;

        let Some(account) = account else {
            warn!(target: COMPONENT, "Network account details for tag {tag} not found in the store");
            return Ok(None);
        };

        Ok(Some(account.id()))
    }

    /// Filters the [`NetworkTransactionRequest`]'s notes by making one consumption check against
    /// the executing account.
    #[instrument(target = COMPONENT, name = "ntx_builder.filter_consumable_notes", skip_all, err)]
    async fn filter_consumable_notes(
        data_store: &Arc<NtxBuilderDataStore>,
        tx_executor: &TransactionExecutor,
        tx_request: &NetworkTransactionRequest,
    ) -> Result<NetworkTransactionRequest, NtxBuilderError> {
        let input_notes = InputNotes::new(
            tx_request
                .notes_to_execute
                .iter()
                .cloned()
                .map(Note::from)
                .map(InputNote::unauthenticated)
                .collect(),
        )?;

        for note in input_notes.iter() {
            data_store.insert_note_script_mast(note.note().script());
        }

        let checker = NoteConsumptionChecker::new(tx_executor);
        match checker
            .check_notes_consumability(
                tx_request.account_id,
                tx_request.block_ref,
                input_notes.clone(),
                TransactionArgs::default(),
                Arc::new(DefaultSourceManager::default()),
            )
            .await
        {
            Ok(NoteAccountExecution::Success) => Ok(tx_request.clone()),
            Ok(NoteAccountExecution::Failure { successful_notes, error, failed_note_id }) => {
                let successful_network_notes: Vec<NetworkNote> = input_notes
                    .iter()
                    .filter(|n| successful_notes.contains(&n.id()))
                    .map(InputNote::note)
                    .cloned()
                    .map(|n| NetworkNote::try_from(n).expect("conversion should work"))
                    .collect();

                if let Some(ref err) = error {
                    Span::current()
                        .set_attribute("ntx.consumption_check_error", err.to_string().as_str());
                } else {
                    Span::current().set_attribute("ntx.consumption_check_error", "none");
                }
                Span::current()
                    .set_attribute("ntx.failed_note_id", failed_note_id.to_hex().as_str());

                if successful_network_notes.is_empty() {
                    return Err(NtxBuilderError::NoteSetIsEmpty(tx_request.account_id));
                }

                Ok(NetworkTransactionRequest::new(
                    tx_request.account_id,
                    tx_request.block_ref,
                    successful_network_notes,
                ))
            },
            Err(err) => Err(NtxBuilderError::NoteConsumptionCheckFailed(err)),
        }
    }

    /// Executes the transaction with the account described by the request.
    #[instrument(target = COMPONENT, name = "ntx_builder.execute_transaction", skip_all, err)]
    async fn execute_transaction(
        tx_executor: &TransactionExecutor,
        tx_request: NetworkTransactionRequest,
    ) -> Result<ExecutedTransaction, NtxBuilderError> {
        let input_notes = InputNotes::new(
            tx_request
                .notes_to_execute
                .iter()
                .cloned()
                .map(Note::from)
                .map(InputNote::unauthenticated)
                .collect(),
        )?;

        tx_executor
            .execute_transaction(
                tx_request.account_id,
                tx_request.block_ref,
                input_notes,
                TransactionArgs::default(),
                Arc::new(DefaultSourceManager::default()),
            )
            .await
            .map_err(NtxBuilderError::ExecutionError)
    }

    /// Proves the transaction and submits it to the mempool.
    #[instrument(target = COMPONENT, name = "ntx_builder.prove_and_submit_transaction", skip_all, err)]
    async fn prove_and_submit_transaction(
        tx_prover: &NtbTransactionProver,
        block_prod: &BlockProducerClient,
        executed_tx: ExecutedTransaction,
    ) -> Result<ExecutedTransaction, NtxBuilderError> {
        tx_prover.prove_and_submit(block_prod, &executed_tx).await?;

        Ok(executed_tx)
    }

    /// Rolls back the transaction. This should be executed if the execution stage of the pipeline
    /// failed. Specifically, this involves requeuing notes and evicting the account from the
    /// cache.
    #[instrument(target = COMPONENT, name = "ntx_builder.rollback_tx", skip_all)]
    async fn rollback_tx(
        tx_request: NetworkTransactionRequest,
        api_state: &SharedPendingNotes,
        data_store: &Arc<NtxBuilderDataStore>,
    ) {
        // Roll back any state changes and re-queue notes if needed
        data_store.evict_account(tx_request.account_id);

        api_state.lock().await.queue_unconsumed_notes(tx_request.notes_to_execute);
    }
}

// BUILDER ERRORS
// =================================================================================================

#[derive(Debug, Error)]
pub enum NtxBuilderError {
    #[error("account cache update error")]
    AccountCacheUpdateFailed(#[from] AccountError),
    #[error("store error")]
    Store(#[from] StoreError),
    #[error("transaction inputs error")]
    TransactionInputError(#[from] TransactionInputError),
    #[error("transaction execution error")]
    ExecutionError(#[source] TransactionExecutorError),
    #[error("error while checking for note consumption compatibility")]
    NoteConsumptionCheckFailed(#[source] TransactionExecutorError),
    #[error("after performing a consumption check for account, the note list became empty")]
    NoteSetIsEmpty(AccountId),
    #[error("block producer client error")]
    BlockProducer(#[from] tonic::Status),
    #[error("network account error")]
    NetworkAccount(#[from] NetworkAccountError),
    #[error("error while proving transaction")]
    ProverError(#[from] TransactionProverError),
    #[error("error while proving transaction")]
    ProofSubmissionFailed(#[source] tonic::Status),
}
