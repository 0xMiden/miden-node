#![allow(dead_code, reason = "WIP")]
#![allow(unused_variables, reason = "WIP")]
#![allow(unreachable_code, reason = "WIP")]
#![allow(clippy::unused_async, reason = "WIP")]

use std::{net::SocketAddr, num::NonZeroUsize, sync::Arc, time::Duration};

use anyhow::Context;
use futures::{TryFutureExt, TryStream, TryStreamExt};
use miden_node_proto::domain::{account::NetworkAccountError, mempool::MempoolEvent};
use miden_node_utils::tracing::OpenTelemetrySpanExt;
use miden_objects::{
    AccountError, TransactionInputError,
    account::AccountId,
    assembly::DefaultSourceManager,
    block::BlockNumber,
    transaction::{ExecutedTransaction, InputNote, InputNotes, TransactionArgs},
};
use miden_tx::{
    NoteAccountExecution, NoteConsumptionChecker, TransactionExecutor, TransactionExecutorError,
    TransactionProverError,
};
use thiserror::Error;
use tokio::{runtime::Builder as RtBuilder, task::spawn_blocking, time};
use tracing::{Instrument, Span, error, info, instrument, warn};
use url::Url;

use crate::{
    COMPONENT,
    block_producer::BlockProducerClient,
    note::NetworkNote,
    prover::NtbTransactionProver,
    state::State,
    store::{StoreClient, StoreError},
};

// NETWORK TRANSACTION REQUEST
// ================================================================================================

#[derive(Clone, Debug)]
struct NetworkTransactionRequest {
    account_id: AccountId,
    notes_to_execute: Vec<NetworkNote>,
}

impl NetworkTransactionRequest {
    fn new(account_id: AccountId, notes: Vec<NetworkNote>) -> Self {
        Self { account_id, notes_to_execute: notes }
    }

    pub fn input_notes(&self) -> Result<InputNotes<InputNote>, TransactionInputError> {
        let notes = self
            .notes_to_execute
            .iter()
            .map(NetworkNote::inner)
            .cloned()
            .map(InputNote::unauthenticated)
            .collect();

        InputNotes::new(notes)
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
        let block_prod = BlockProducerClient::new(self.block_producer_address);
        let tx_prover = NtbTransactionProver::from(self.tx_prover_url.clone());
        let mut interval = tokio::time::interval(self.ticker_interval);

        // This extra runtime is required to work-around miden-base objects not being Send/Sync due
        // to dyn Trait object usage.
        spawn_blocking(move || {
            let rt = RtBuilder::new_current_thread()
                .enable_all()
                .build()
                .context("failed to build runtime")?;

            rt.block_on(async move {
                let (mut state, mut stream) = Self::init(&store, &block_prod)
                    .await
                    .context("failed to initialize the ntx builder")?;

                loop {
                    tokio::select! {
                        _tick = interval.tick() => match Self::build_network_tx(&state, &tx_prover, &block_prod).await {
                            Ok(_) => todo!(),
                            Err(_) => todo!(),
                        },
                        mempool_event = stream.try_next() => {
                            match mempool_event {
                                Ok(Some(event)) => state.update(event),
                                Ok(None) => anyhow::bail!("mempool event stream ended"),
                                Err(err) => return Err(err).context("mempool event stream encountered an error"),
                            }
                        },
                    }
                }
            })
        }).await.context("joining ntx builder runtime")?
    }

    async fn init(
        store: &StoreClient,
        block_prod: &BlockProducerClient,
    ) -> anyhow::Result<(State, impl TryStream<Ok = MempoolEvent, Error = tonic::Status>)> {
        let mut state = State::new(todo!());
        loop {
            state.sync_committed(store).await.context("failed to sync state with store")?;
            let chain_tip = state.chain_tip();

            match block_prod.subscribe_to_mempool(chain_tip).await {
                Ok(stream) => return Ok((state, stream)),
                Err(desync) if desync.code() == tonic::Code::InvalidArgument => {
                    tracing::warn!("not yet in sync with mempool");
                },
                Err(retry) if retry.code() == tonic::Code::Unavailable => {
                    tracing::warn!(
                        err = retry.message(),
                        "mempool event subscription unavailable, retrying"
                    );
                    tokio::time::sleep(Duration::from_secs(5)).await;
                },
                Err(err) => return Err(err).context("failed to subscribe to mempool events"),
            }
        }
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
        state: &State,
        prover: &NtbTransactionProver,
        block_prod: &BlockProducerClient,
    ) -> Result<Option<ExecutedTransaction>, NtxBuilderError> {
        // Preflight: Look for next account and blockchain data, and select notes
        let Some(tx_request) = Self::select_next_tx(state).await else {
            return Ok(None);
        };

        // Execution: Filter notes, execute, prove and submit tx
        let tx = Self::filter_consumable_notes(state, tx_request)
            .and_then(|filtered_tx_req| Self::execute_transaction(state, filtered_tx_req))
            .and_then(|executed_tx| Self::prove_and_submit_transaction(prover, block_prod, executed_tx))
            .inspect_ok(|tx| {
                info!(target: COMPONENT, tx_id = %tx.id(), "Proved and submitted network transaction");
            })
            .inspect_err(|err| {
                warn!(target: COMPONENT, error = %err, "Error in transaction processing");
                Span::current().set_error(err);
            })
            .instrument(Span::current())
            .await?;

        Ok(Some(tx))
    }

    /// Selects the next tag and set of notes to execute.
    /// If a tag is in queue, we attempt to retrieve the account and update the datastore's partial
    /// MMR.
    ///
    /// If this function errors, the notes are effectively discarded because [`Self::rollback_tx()`]
    /// is not called.
    async fn select_next_tx(_state: &State) -> Option<NetworkTransactionRequest> {
        todo!()
    }

    /// Filters the [`NetworkTransactionRequest`]'s notes by making one consumption check against
    /// the executing account.
    #[instrument(target = COMPONENT, name = "ntx_builder.filter_consumable_notes", skip_all, err)]
    async fn filter_consumable_notes(
        state: &State,
        tx_request: NetworkTransactionRequest,
    ) -> Result<NetworkTransactionRequest, NtxBuilderError> {
        let executor = TransactionExecutor::new(state, None);
        let checker = NoteConsumptionChecker::new(&executor);
        let notes = tx_request.input_notes()?;
        // TODO: It seems a bit silly that the checker consumes the notes but returns the note IDs.
        match checker
            .check_notes_consumability(
                tx_request.account_id,
                BlockNumber::GENESIS,
                notes.clone(),
                TransactionArgs::default(),
                Arc::new(DefaultSourceManager::default()),
            )
            .await
        {
            Ok(NoteAccountExecution::Success) => Ok(tx_request),
            Ok(NoteAccountExecution::Failure { successful_notes, error, failed_note_id }) => {
                let notes = successful_notes
                    .into_iter()
                    .map(|id| notes.iter().find(|note| note.id() == id).expect("note must be part of input set"))
                    .map(|note| note.note().clone())
                    // SAFETY: all input notes were network notes
                    .map(NetworkNote::unchecked)
                    .collect::<Vec<_>>();

                if let Some(ref err) = error {
                    Span::current()
                        .set_attribute("ntx.consumption_check_error", err.to_string().as_str());
                } else {
                    Span::current().set_attribute("ntx.consumption_check_error", "none");
                }
                Span::current()
                    .set_attribute("ntx.failed_note_id", failed_note_id.to_hex().as_str());

                if notes.is_empty() {
                    return Err(NtxBuilderError::NoteSetIsEmpty(tx_request.account_id));
                }

                Ok(NetworkTransactionRequest::new(tx_request.account_id, notes))
            },
            Err(err) => Err(NtxBuilderError::NoteConsumptionCheckFailed(err)),
        }
    }

    /// Executes the transaction with the account described by the request.
    #[instrument(target = COMPONENT, name = "ntx_builder.execute_transaction", skip_all, err)]
    async fn execute_transaction(
        state: &State,
        tx_request: NetworkTransactionRequest,
    ) -> Result<ExecutedTransaction, NtxBuilderError> {
        let input_notes = InputNotes::new(
            tx_request
                .notes_to_execute
                .iter()
                .map(NetworkNote::inner)
                .cloned()
                .map(InputNote::unauthenticated)
                .collect(),
        )?;

        let executor = TransactionExecutor::new(state, None);

        executor
            .execute_transaction(
                tx_request.account_id,
                BlockNumber::GENESIS,
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
        tx_prover.prove_and_submit(block_prod, executed_tx.clone()).await?;

        Ok(executed_tx)
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
