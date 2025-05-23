use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::Context;
use block_producer::BlockProducerClient;
use data_store::NtxBuilderDataStore;
use miden_node_proto::generated::ntx_builder::api_server;
use miden_node_utils::network_note::NetworkNote;
use miden_objects::{
    assembly::DefaultSourceManager,
    note::Note,
    transaction::{
        ExecutedTransaction, InputNote, InputNotes, TransactionArgs, TransactionWitness,
    },
};
use miden_proving_service_client::proving_service::tx_prover::RemoteTransactionProver;
use miden_tx::{
    LocalTransactionProver, NoteAccountExecution, NoteConsumptionChecker, ProvingOptions,
    TransactionExecutor, TransactionProver,
};
use server::{NtxBuilderApi, PendingNotes};
use store::StoreClient;
use tokio::{
    net::TcpListener,
    runtime::Builder as RtBuilder,
    sync::Mutex,
    task::{JoinHandle, spawn_blocking},
    time,
};
use tokio_stream::wrappers::TcpListenerStream;
use tower_http::trace::TraceLayer;
use tracing::{debug, info, warn};
use url::Url;

use crate::COMPONENT;

mod block_producer;
mod data_store;
mod server;
mod store;

type SharedPendingNotes = Arc<Mutex<PendingNotes>>;

/// Interval for checking pending notes
const NOTE_CHECK_INTERVAL_MS: u64 = 200;

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
    /// which is undesirable.
    pub proving_service_address: Option<Url>,
}
impl NetworkTransactionBuilder {
    /// Serves the transaction builder service.
    ///
    /// If for any reason the service errors, it gets restarted.
    pub async fn serve_resilient(&mut self) -> anyhow::Result<()> {
        loop {
            match self.serve_once().await {
                Ok(()) => warn!(target: COMPONENT, "ntx-builder stopped without error, restarting"),
                Err(e) => warn!(target: COMPONENT, error = %e, "ntx-builder crashed, restarting"),
            }

            // sleep before retrying to not spin the server
            time::sleep(Duration::from_secs(5)).await;
        }
    }

    pub async fn serve_once(&self) -> anyhow::Result<()> {
        let listener = TcpListener::bind(self.ntx_builder_address).await?;
        info!(
            target: COMPONENT,
            endpoint = ?listener.local_addr()?,
            "Starting network transaction builder server"
        );

        let store = StoreClient::new(&self.store_url);

        // Initialize unconsumed notes
        let unconsumed_network_notes = store.get_unconsumed_network_notes().await?;
        let notes_queue: SharedPendingNotes =
            Arc::new(Mutex::new(PendingNotes::new(unconsumed_network_notes)));

        // Initialize tx ticker
        let tick_handle = self.spawn_ticker(notes_queue.clone());

        // Initialize grpc server
        let api = NtxBuilderApi::new(notes_queue);
        let api_service = api_server::ApiServer::new(api);
        let server_result = tonic::transport::Server::builder()
            .accept_http1(true)
            .layer(TraceLayer::new_for_grpc())
            .add_service(api_service)
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await;

        tick_handle.abort();

        server_result.context("failed to serve network transaction builder API")
    }

    /// Spawns the ticker task and returns a handle to it.
    ///
    /// The ticker is in charge of periodically checking the network notes set and executing the
    /// next set of notes.
    fn spawn_ticker(&self, api_state: SharedPendingNotes) -> JoinHandle<anyhow::Result<()>> {
        let store_url = self.store_url.clone();
        let block_addr = self.block_producer_address;
        let prover_addr = self.proving_service_address.clone();

        spawn_blocking(move || {
            let rt = RtBuilder::new_current_thread()
                .enable_all()
                .build()
                .context("failed to build runtime")?;

            rt.block_on(async move {
                let store = StoreClient::new(&store_url);
                let data_store = Arc::new(NtxBuilderDataStore::new(store).await?);
                let tx_executor = TransactionExecutor::new(data_store.clone(), None);
                let block_prod = BlockProducerClient::new(block_addr);
                let remote_tx_prover =
                    prover_addr.map(|uri| RemoteTransactionProver::new(uri.to_string()));

                let mut interval = time::interval(Duration::from_millis(NOTE_CHECK_INTERVAL_MS));

                loop {
                    interval.tick().await;

                    let tx = filter_next_and_execute(&api_state, &tx_executor, &data_store).await?;
                    if let Some(tx) = &tx {
                        let proof_result = prove_and_submit(&api_state, &block_prod, tx, remote_tx_prover.as_ref()).await;
                        if let Err(err) = proof_result {
                            warn!(target: COMPONENT,err = %err,"Error proving and submitting transaction");

                            api_state.lock().await.rollback_inflight(tx.id());
                            data_store.evict_account(tx.account_id());
                        } else {
                            info!(target: COMPONENT, tx_id = %tx.id(),"Proved and submitted network transaction");

                            data_store.update_account(tx).context("error updating account in the cache")?;
                        }
                    }
                }
            })
        })
    }
}

/// Gets the next set of notes to execute, filters by using a consumption checker and executes a
/// transaction that consumes them.
///
/// This function deals with errors internally and rolls back the state when required.
async fn filter_next_and_execute(
    api_state: &SharedPendingNotes,
    tx_executor: &TransactionExecutor,
    data_store: &Arc<NtxBuilderDataStore>,
) -> anyhow::Result<Option<ExecutedTransaction>> {
    let (tag, notes) = {
        let mut queue = api_state.lock().await;
        if let Some((tag, notes)) = queue.take_next_notes_by_tag() {
            debug!(target: COMPONENT, tag=%tag, "Notes with tag found for processing");
            (tag, notes)
        } else {
            debug!(target: COMPONENT, "No unprocessed notes found");
            return Ok(None);
        }
    };

    let block_ref = data_store
        .update_blockchain_data()
        .await
        .context("Failed to update blockchain data")?;
    let account = data_store
        .get_cached_acc_or_fetch_by_tag(tag)
        .await
        .context("Failed to retrieve account data")?;

    let Some(account) = account else {
        // re-add notes and return because the account was not found in the cache nor the store
        warn!(target:COMPONENT, tag=%tag, "No account was found for network note tag, returning");
        // TODO: do we want to completely remove these notes from the list?
        api_state.lock().await.queue_unconsumed_notes(notes);
        return Ok(None);
    };

    // select input notes by running them through the checker
    let input_notes = {
        let input_notes = InputNotes::new(
            notes.iter().cloned().map(Note::from).map(InputNote::unauthenticated).collect(),
        )
        .context("failed to create InputNotes object")?;

        for n in input_notes.iter() {
            data_store.insert_note_script_mast(n.note().script());
        }

        let checker = NoteConsumptionChecker::new(tx_executor);
        match checker
            .check_notes_consumability(
                account.id(),
                block_ref,
                input_notes.clone(),
                TransactionArgs::default(),
                Arc::new(DefaultSourceManager::default()),
            )
            .await
        {
            Ok(NoteAccountExecution::Success) => input_notes,
            Ok(NoteAccountExecution::Failure { successful_notes, .. }) => {
                // Take only successful notes, put failing notes back to the queue
                let successul_notes =
                    input_notes.iter().filter(|n| successful_notes.contains(&n.id())).cloned();

                let unsuccessful_notes: Vec<NetworkNote> = input_notes
                    .iter()
                    .filter(|n| !successful_notes.contains(&n.id()))
                    .map(InputNote::note)
                    .cloned()
                    .map(|n| NetworkNote::try_from(n).expect("checked above"))
                    .collect();

                api_state
                    .lock()
                    .await
                    .queue_unconsumed_notes(unsuccessful_notes.into_iter().rev());

                InputNotes::new(successul_notes.collect()).unwrap()
            },
            Err(err) => {
                warn!(target: COMPONENT, error=%err, "Consumption check failed");
                api_state.lock().await.queue_unconsumed_notes(notes);
                data_store.evict_account(account.id());
                return Ok(None);
            },
        }
    };

    if input_notes.iter().count() == 0 {
        warn!(target: COMPONENT, "No notes left to be consumed after consumption check");

        return Ok(None);
    }

    match tx_executor
        .execute_transaction(
            account.id(),
            block_ref,
            input_notes,
            TransactionArgs::default(),
            Arc::new(DefaultSourceManager::default()),
        )
        .await
    {
        Ok(tx) => Ok(Some(tx)),
        Err(err) => {
            warn!(target: COMPONENT, error=%err, "Network transaction execution failed");
            api_state.lock().await.queue_unconsumed_notes(notes);
            data_store.evict_account(account.id());

            Ok(None)
        },
    }
}

/// Proves and submit an executed transaction
async fn prove_and_submit(
    api_state: &SharedPendingNotes,
    block_producer_client: &BlockProducerClient,
    executed_tx: &ExecutedTransaction,
    remote_tx_prover: Option<&RemoteTransactionProver>,
) -> anyhow::Result<()> {
    api_state.lock().await.mark_inflight(
        executed_tx.id(),
        executed_tx
            .input_notes()
            .iter()
            .map(|n| {
                NetworkNote::try_from(n.note().clone())
                    .expect("any executed note was checked to be network")
            })
            .collect(),
    );

    let tx_witness = TransactionWitness::from(executed_tx.clone());
    let proven_tx = if let Some(tx_prover) = remote_tx_prover {
        tx_prover.prove(tx_witness).await
        // TODO: should we fall back to a local tx prover here?
    } else {
        let local_tx_prover = LocalTransactionProver::new(ProvingOptions::default());
        local_tx_prover.prove(tx_witness).await
    }
    .context("failed to prove transaction")?;

    block_producer_client
        .submit_proven_network_transaction(proven_tx)
        .await
        .context("Failed to submit proven tx")
}
