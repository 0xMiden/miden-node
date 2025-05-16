use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::Context;
use block_producer::BlockProducerClient;
use data_store::NtxBuilderDataStore;
use miden_node_proto::generated::ntx_builder::api_server;
use miden_objects::transaction::{InputNote, InputNotes, TransactionArgs, TransactionWitness};
use miden_tx::{LocalTransactionProver, ProvingOptions, TransactionExecutor, TransactionProver};
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

use crate::COMPONENT;

mod block_producer;
mod data_store;
mod server;
mod store;

/// Interval for checking pending notes
const NOTE_CHECK_INTERVAL_MS: u64 = 200;

/// Network Transaction Builder
pub struct NetworkTransactionBuilder {
    /// The listener for the network transaction builder gRPC server.
    pub ntx_builder_listener: TcpListener,
    /// Address of the store gRPC server.
    pub store_address: SocketAddr,
    /// Address of the block producer gRPC server.
    pub block_producer_address: SocketAddr,
}

impl NetworkTransactionBuilder {
    pub async fn serve(self) -> anyhow::Result<()> {
        info!(target: COMPONENT, endpoint=?self.ntx_builder_listener.local_addr().unwrap(), "Starting network transaction builder server");

        let store = StoreClient::new(self.store_address);
        let unconsumed_network_notes = store.get_unconsumed_network_notes().await?;
        let api = NtxBuilderApi::new(unconsumed_network_notes);
        let notes_queue = api.state();

        let api_service = api_server::ApiServer::new(api);

        let tick_handle =
            Self::spawn_ticker(notes_queue, self.store_address, self.block_producer_address);
        println!("ererer3");

        let server_result = tonic::transport::Server::builder()
            .accept_http1(true)
            .layer(TraceLayer::new_for_grpc())
            .add_service(api_service)
            .serve_with_incoming(TcpListenerStream::new(self.ntx_builder_listener))
            .await;

        tick_handle.abort();

        server_result.context("failed to serve network transaction builder API")
    }

    /// Spawns the ticker task and returns a a handle to it.
    ///
    /// The ticker is in charge of periodically checking the network notes set and executing the
    /// next set of notes
    fn spawn_ticker(
        api_state: Arc<Mutex<PendingNotes>>,
        store_address: SocketAddr,
        block_producer_address: SocketAddr,
    ) -> JoinHandle<()> {
        spawn_blocking(move || {
            let rt = RtBuilder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed when building runtime");

            rt.block_on(async move {
                let store = StoreClient::new(store_address);
                let block_producer = BlockProducerClient::new(block_producer_address);

                let data_store = Arc::new(NtxBuilderDataStore::new(store).await.unwrap());
                let tx_executor = TransactionExecutor::new(data_store.clone(), None);
                let mut interval = time::interval(Duration::from_millis(NOTE_CHECK_INTERVAL_MS));

                loop {
                    interval.tick().await;
                    let mut notes_queue = api_state.lock().await;

                    if notes_queue.has_pending() {
                        if let Some((tag, notes)) = notes_queue.take_next_notes_by_tag() {
                            debug!(target: COMPONENT, "Found notes to process");

                            // load datastore with required data: MMR-data and update cache
                            // to include the network account
                            let current_block_number = data_store.update_blockchain_data().await.unwrap();
                            let acc = data_store.get_cached_acc_or_fetch_by_tag(tag).await.unwrap();

                            // SAFETY: we take a limited amount of notes from the state
                            let input_notes = InputNotes::new(notes.iter().cloned().map(InputNote::unauthenticated).collect()).unwrap();
                            let executed_tx_result = tx_executor.execute_transaction(acc.id(), current_block_number, input_notes, TransactionArgs::default()).await;

                            match executed_tx_result {
                                Ok(tx) => {
                                    // TODO: we could drop the lock here
                                    notes_queue.mark_inflight(tx.id() ,notes);
                                    let tx_prover = LocalTransactionProver::new(ProvingOptions::default());
                                    // TODO: add delegated proving here
                                    let proven_tx = tx_prover.prove(TransactionWitness::from(tx.clone())).await.unwrap();

                                    _ = block_producer.submit_proven_network_transaction(proven_tx).await.map_err(|err| {
                                        warn!(target: COMPONENT, error=%err, "error submitting transaction");
                                        notes_queue.rollback_inflight(tx.id());
                                        data_store.evict_account(acc.id());
                                    });
                                },
                                Err(err) => {
                                    // re-add notes
                                    warn!(target: COMPONENT, error=%err, "error executing transaction");

                                    notes_queue.add_unconsumed_notes(notes);
                                    data_store.evict_account(acc.id());
                                },
                            }
                        }
                    } else {
                        debug!(target: COMPONENT, "No unprocessed notes found");

                    }
                }
            });
        })
    }
}
