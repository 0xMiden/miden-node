use std::{collections::HashMap, net::SocketAddr, time::Duration};

use anyhow::{Context, Result};
use miden_node_proto::{
    generated::{
        block_producer::api_server,
        requests::SubmitProvenTransactionRequest,
        responses::{BlockProducerStatusResponse, SubmitProvenTransactionResponse},
    },
    ntx_builder,
};
use miden_node_utils::{
    formatting::{format_input_notes, format_output_notes},
    tracing::grpc::{OtelInterceptor, block_producer_trace_fn},
};
use miden_objects::{
    note::Nullifier, transaction::ProvenTransaction, utils::serde::Deserializable,
};
use tokio::{net::TcpListener, sync::Mutex};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::Status;
use tower_http::trace::TraceLayer;
use tracing::{debug, error, info, instrument, warn};
use url::Url;

use crate::{
    COMPONENT, SERVER_MEMPOOL_EXPIRATION_SLACK, SERVER_MEMPOOL_STATE_RETENTION,
    SERVER_NUM_BATCH_BUILDERS,
    batch_builder::BatchBuilder,
    block_builder::{BlockBuilder, NtxClient},
    domain::transaction::AuthenticatedTransaction,
    errors::{AddTransactionError, BlockProducerError, StoreError, VerifyTxError},
    mempool::{BatchBudget, BlockBudget, Mempool, SharedMempool},
    store::StoreClient,
};

/// The block producer server.
///
/// Specifies how to connect to the store, batch prover, and block prover components.
/// The connection to the store is established at startup and retried with exponential backoff
/// until the store becomes available. Once the connection is established, the block producer
/// will start serving requests.
/// The block producer server.
///
/// Specifies how to connect to the store, batch prover, and block prover components.
/// The connection to the store is established at startup and retried with exponential backoff
/// until the store becomes available. Once the connection is established, the block producer
/// will start serving requests.
pub struct BlockProducer {
    /// The address of the block producer component.
    pub block_producer_address: SocketAddr,
    /// The address of the store component.
    pub store_address: SocketAddr,
    /// The address of the network transaction builder.
    pub ntx_builder_address: SocketAddr,
    /// The address of the batch prover component.
    pub batch_prover_url: Option<Url>,
    /// The address of the block prover component.
    pub block_prover_url: Option<Url>,
    /// The interval at which to produce batches.
    pub batch_interval: Duration,
    /// The interval at which to produce blocks.
    pub block_interval: Duration,
}

impl BlockProducer {
    /// Serves the block-producer RPC API, the batch-builder and the block-builder.
    ///
    /// Note: Executes in place (i.e. not spawned) and will run indefinitely until
    ///       a fatal error is encountered.
    pub async fn serve(self) -> anyhow::Result<()> {
        info!(target: COMPONENT, endpoint=?self.block_producer_address, store=%self.store_address, "Initializing server");
        let store = StoreClient::new(self.store_address);

        // retry fetching the chain tip from the store until it succeeds.
        let mut retries_counter = 0;
        let chain_tip = loop {
            match store.latest_header().await {
                Err(StoreError::GrpcClientError(err)) => {
                    // exponential backoff with base 500ms and max 30s
                    let backoff = Duration::from_millis(500)
                        .saturating_mul(1 << retries_counter)
                        .min(Duration::from_secs(30));

                    error!(
                        store = %self.store_address,
                        ?backoff,
                        %retries_counter,
                        %err,
                        "store connection failed while fetching chain tip, retrying"
                    );

                    retries_counter += 1;
                    tokio::time::sleep(backoff).await;
                },
                Ok(header) => break header.block_num(),
                Err(e) => {
                    error!(target: COMPONENT, %e, "failed to fetch chain tip from store");
                    return Err(e.into());
                },
            }
        };

        let listener = TcpListener::bind(self.block_producer_address)
            .await
            .context("failed to bind to block producer address")?;

        let ntx_client =
            ntx_builder::Client::lazy_with_interceptor(self.ntx_builder_address, OtelInterceptor);

        info!(target: COMPONENT, "Server initialized");

        let block_builder = BlockBuilder::new(
            store.clone(),
            ntx_client,
            self.block_prover_url,
            self.block_interval,
        );
        let batch_builder = BatchBuilder::new(
            store.clone(),
            SERVER_NUM_BATCH_BUILDERS,
            self.batch_prover_url,
            self.batch_interval,
        );
        let mempool = Mempool::shared(
            chain_tip,
            BatchBudget::default(),
            BlockBudget::default(),
            SERVER_MEMPOOL_STATE_RETENTION,
            SERVER_MEMPOOL_EXPIRATION_SLACK,
        );

        // Spawn rpc server and batch and block provers.
        //
        // These communicate indirectly via a shared mempool.
        //
        // These should run forever, so we combine them into a joinset so that if
        // any complete or fail, we can shutdown the rest (somewhat) gracefully.
        let mut tasks = tokio::task::JoinSet::new();

        let batch_builder_id = tasks
            .spawn({
                let mempool = mempool.clone();
                async {
                    batch_builder.run(mempool).await;
                    Ok(())
                }
            })
            .id();
        let block_builder_id = tasks
            .spawn({
                let mempool = mempool.clone();
                async {
                    block_builder.run(mempool).await;
                    Ok(())
                }
            })
            .id();

        let ntx_client =
            ntx_builder::Client::lazy_with_interceptor(self.ntx_builder_address, OtelInterceptor);
        let rpc_id = tasks
            .spawn(async move {
                BlockProducerRpcServer::new(mempool, store, ntx_client).serve(listener).await
            })
            .id();

        let task_ids = HashMap::from([
            (batch_builder_id, "batch-builder"),
            (block_builder_id, "block-builder"),
            (rpc_id, "rpc"),
        ]);

        // Wait for any task to end. They should run indefinitely, so this is an unexpected result.
        //
        // SAFETY: The JoinSet is definitely not empty.
        let task_result = tasks.join_next_with_id().await.unwrap();

        let task_id = match &task_result {
            Ok((id, _)) => *id,
            Err(err) => err.id(),
        };
        let task = task_ids.get(&task_id).unwrap_or(&"unknown");

        // We could abort the other tasks here, but not much point as we're probably crashing the
        // node.
        task_result
            .map_err(|source| BlockProducerError::JoinError { task, source })
            .map(|(_, result)| match result {
                Ok(_) => Err(BlockProducerError::TaskFailedSuccesfully { task }),
                Err(source) => Err(BlockProducerError::TonicTransportError { task, source }),
            })
            .and_then(|x| x)?
    }
}

/// Serves the block producer's RPC [api](api_server::Api).
struct BlockProducerRpcServer {
    /// The mutex effectively rate limits incoming transactions into the mempool by forcing them
    /// through a queue.
    ///
    /// This gives mempool users such as the batch and block builders equal footing with __all__
    /// incoming transactions combined. Without this incoming transactions would greatly restrict
    /// the block-producers usage of the mempool.
    mempool: Mutex<SharedMempool>,

    store: StoreClient,

    ntx_client: NtxClient,
}

#[tonic::async_trait]
impl api_server::Api for BlockProducerRpcServer {
    async fn submit_proven_transaction(
        &self,
        request: tonic::Request<SubmitProvenTransactionRequest>,
    ) -> Result<tonic::Response<SubmitProvenTransactionResponse>, Status> {
        self.submit_proven_transaction(request.into_inner())
            .await
            .map(tonic::Response::new)
            // This Status::from mapping takes care of hiding internal errors.
            .map_err(Into::into)
    }

    #[instrument(
        target = COMPONENT,
        name = "block_producer.server.status",
        skip_all,
        err
    )]
    async fn status(
        &self,
        _request: tonic::Request<()>,
    ) -> Result<tonic::Response<BlockProducerStatusResponse>, Status> {
        Ok(tonic::Response::new(BlockProducerStatusResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
            status: "connected".to_string(),
        }))
    }
}

impl BlockProducerRpcServer {
    pub fn new(mempool: SharedMempool, store: StoreClient, ntx_client: NtxClient) -> Self {
        Self {
            mempool: Mutex::new(mempool),
            store,
            ntx_client,
        }
    }

    async fn serve(self, listener: TcpListener) -> Result<(), tonic::transport::Error> {
        // Build the gRPC server with the API service and trace layer.
        tonic::transport::Server::builder()
            .layer(TraceLayer::new_for_grpc().make_span_with(block_producer_trace_fn))
            .add_service(api_server::ApiServer::new(self))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
    }

    #[instrument(
        target = COMPONENT,
        name = "block_producer.server.submit_proven_transaction",
        skip_all,
        err
    )]
    async fn submit_proven_transaction(
        &self,
        request: SubmitProvenTransactionRequest,
    ) -> Result<SubmitProvenTransactionResponse, AddTransactionError> {
        debug!(target: COMPONENT, ?request);

        let tx = ProvenTransaction::read_from_bytes(&request.transaction)
            .map_err(AddTransactionError::TransactionDeserializationFailed)?;

        let tx_id = tx.id();

        info!(
            target: COMPONENT,
            tx_id = %tx_id.to_hex(),
            account_id = %tx.account_id().to_hex(),
            initial_state_commitment = %tx.account_update().initial_state_commitment(),
            final_state_commitment = %tx.account_update().final_state_commitment(),
            input_notes = %format_input_notes(tx.input_notes()),
            output_notes = %format_output_notes(tx.output_notes()),
            ref_block_commitment = %tx.ref_block_commitment(),
            "Deserialized transaction"
        );
        debug!(target: COMPONENT, proof = ?tx.proof());

        let inputs = self.store.get_tx_inputs(&tx).await.map_err(VerifyTxError::from)?;

        // SAFETY: we assume that the rpc component has verified the transaction proof already.
        let tx_id = tx.id();
        let tx_nullifiers: Vec<Nullifier> = tx.nullifiers().collect();

        let tx = AuthenticatedTransaction::new(tx, inputs)?;

        // Launch a task for updating the mempool, and send the update to the network transaction
        // builder
        let (submit_tx_response, _) = tokio::join!(
            async {
                self.mempool.lock().await.lock().await.add_transaction(tx).map(|block_height| {
                    SubmitProvenTransactionResponse { block_height: block_height.as_u32() }
                })
            },
            async {
                let mut ntx_client = self.ntx_client.clone();
                if let Err(err) =
                    ntx_client.update_network_notes(tx_id, tx_nullifiers.into_iter()).await
                {
                    warn!(
                        target: COMPONENT,
                        message = %err,
                        "error submitting network notes updates to ntx builder"
                    );
                }
            },
        );

        submit_tx_response
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use miden_air::{ExecutionProof, HashFunction};
    use miden_node_proto::generated::{
        block_producer::api_client as block_producer_client,
        requests::SubmitProvenTransactionRequest, responses::SubmitProvenTransactionResponse,
    };
    use miden_node_store::{GenesisState, Store};
    use miden_objects::{
        Digest,
        account::{AccountId, AccountIdVersion, AccountStorageMode, AccountType},
        transaction::ProvenTransactionBuilder,
    };
    use miden_tx::utils::Serializable;
    use tokio::{net::TcpListener, runtime, task, time::sleep};
    use tonic::transport::{Channel, Endpoint};
    use winterfell::Proof;

    use crate::BlockProducer;

    #[tokio::test]
    async fn block_producer_startup_is_robust_to_network_failures() {
        // This test starts the block producer and tests that it starts serving only after the store
        // is started.

        // get the addresses for the store and block producer
        let store_addr = {
            let store_listener =
                TcpListener::bind("127.0.0.1:0").await.expect("store should bind a port");
            store_listener.local_addr().expect("store should get a local address")
        };
        let block_producer_addr = {
            let block_producer_listener =
                TcpListener::bind("127.0.0.1:0").await.expect("failed to bind block-producer");
            block_producer_listener
                .local_addr()
                .expect("Failed to get block-producer address")
        };

        let ntx_builder_address = {
            let ntx_builder_address = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("failed to bind the ntx builder address");
            ntx_builder_address.local_addr().expect("Failed to get ntx builder address")
        };

        // start the block producer
        task::spawn(async move {
            BlockProducer {
                block_producer_address: block_producer_addr,
                store_address: store_addr,
                ntx_builder_address,
                batch_prover_url: None,
                block_prover_url: None,
                batch_interval: Duration::from_millis(500),
                block_interval: Duration::from_millis(500),
            }
            .serve()
            .await
            .unwrap();
        });

        // test: connecting to the block producer should fail until the store is started
        let block_producer_endpoint =
            Endpoint::try_from(format!("http://{block_producer_addr}")).expect("valid url");
        let block_producer_client =
            block_producer_client::ApiClient::connect(block_producer_endpoint.clone()).await;
        assert!(block_producer_client.is_err());

        // start the store
        let data_directory = tempfile::tempdir().expect("tempdir should be created");
        let store_runtime = {
            let genesis_state = GenesisState::new(vec![], 1, 1);
            Store::bootstrap(genesis_state.clone(), data_directory.path())
                .expect("store should bootstrap");
            let dir = data_directory.path().to_path_buf();
            let store_listener =
                TcpListener::bind(store_addr).await.expect("store should bind a port");
            // in order to later kill the store, we need to spawn a new runtime and run the store on
            // it. That allows us to kill all the tasks spawned by the store when we
            // kill the runtime.
            let store_runtime =
                runtime::Builder::new_multi_thread().enable_time().enable_io().build().unwrap();
            store_runtime.spawn(async move {
                Store {
                    listener: store_listener,
                    data_directory: dir,
                }
                .serve()
                .await
                .expect("store should start serving");
            });
            store_runtime
        };
        // we need to wait for the exponential backoff of the block producer to connect to the store
        sleep(Duration::from_secs(1)).await;

        let block_producer_client =
            block_producer_client::ApiClient::connect(block_producer_endpoint)
                .await
                .expect("block producer client should connect");

        // test: request against block-producer api should succeed
        let response = send_request(block_producer_client.clone(), 0).await;
        assert!(response.is_ok());

        // kill the store
        store_runtime.shutdown_background();

        // test: request against block-producer api should fail immediately
        let response = send_request(block_producer_client.clone(), 1).await;
        assert!(response.is_err());

        // test: restart the store and request should succeed
        let store_listener = TcpListener::bind(store_addr).await.expect("store should bind a port");
        task::spawn(async move {
            Store {
                listener: store_listener,
                data_directory: data_directory.path().to_path_buf(),
            }
            .serve()
            .await
            .expect("store should start serving");
        });
        let response = send_request(block_producer_client.clone(), 2).await;
        assert!(response.is_ok());
    }

    /// Creates a dummy transaction and submits it to the block producer.
    async fn send_request(
        mut client: block_producer_client::ApiClient<Channel>,
        i: u8,
    ) -> Result<tonic::Response<SubmitProvenTransactionResponse>, tonic::Status> {
        let tx = ProvenTransactionBuilder::new(
            AccountId::dummy(
                [0; 15],
                AccountIdVersion::Version0,
                AccountType::RegularAccountImmutableCode,
                AccountStorageMode::Private,
            ),
            Digest::default(),
            [i; 32].try_into().unwrap(),
            0.into(),
            Digest::default(),
            u32::MAX.into(),
            ExecutionProof::new(Proof::new_dummy(), HashFunction::default()),
        )
        .build()
        .unwrap();
        let request = SubmitProvenTransactionRequest { transaction: tx.to_bytes() };
        client.submit_proven_transaction(request).await
    }
}
