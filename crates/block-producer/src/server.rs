use std::{collections::HashMap, net::SocketAddr, time::Duration};

use anyhow::{Context, Result};
use miden_node_proto::generated::{
    block_producer::api_server, requests::SubmitProvenTransactionRequest,
    responses::SubmitProvenTransactionResponse,
};
use miden_node_utils::{
    formatting::{format_input_notes, format_output_notes},
    tracing::grpc::block_producer_trace_fn,
};
use miden_objects::{transaction::ProvenTransaction, utils::serde::Deserializable};
use tokio::{net::TcpListener, sync::Mutex};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::Status;
use tower_http::trace::TraceLayer;
use tracing::{debug, info, instrument};
use url::Url;

use crate::{
    COMPONENT, SERVER_MEMPOOL_EXPIRATION_SLACK, SERVER_MEMPOOL_STATE_RETENTION,
    SERVER_NUM_BATCH_BUILDERS,
    batch_builder::BatchBuilder,
    block_builder::BlockBuilder,
    domain::transaction::AuthenticatedTransaction,
    errors::{AddTransactionError, BlockProducerError, VerifyTxError},
    mempool::{BatchBudget, BlockBudget, Mempool, SharedMempool},
    store::StoreClient,
};

/// Block producer's configuration.
pub struct BlockProducerConfig {
    /// The address of the store component.
    pub store_address: SocketAddr,
    /// The address of the batch prover component.
    pub batch_prover: Option<Url>,
    /// The address of the block prover component.
    pub block_prover: Option<Url>,
    /// The interval at which to produce batches.
    pub batch_interval: Duration,
    /// The interval at which to produce blocks.
    pub block_interval: Duration,
}

/// Serves the block-producer RPC API, the batch-builder and the block-builder.
///
/// Note: this blocks until one of the servers die.
pub async fn serve(listener: TcpListener, config: BlockProducerConfig) -> anyhow::Result<()> {
    info!(target: COMPONENT, endpoint=?listener, store=%config.store_address, "Initializing server");

    let store = StoreClient::new(config.store_address);

    // we drop the tpc listener so that the block producer cannot accept connections until the store
    // is responding
    let block_producer_address = listener.local_addr().context("failed to get address")?;
    drop(listener);

    // this call blocks initialization until the store responds
    let latest_header = store.latest_header().await.context("failed to get latest header")?;
    let chain_tip = latest_header.block_num();

    let listener = TcpListener::bind(block_producer_address)
        .await
        .context("failed to bind to block producer address")?;

    info!(target: COMPONENT, "Server initialized");

    let block_builder =
        BlockBuilder::new(store.clone(), config.block_prover, config.block_interval);
    let batch_builder = BatchBuilder::new(
        store.clone(),
        SERVER_NUM_BATCH_BUILDERS,
        config.batch_prover,
        config.batch_interval,
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
    let rpc_id = tasks
        .spawn(async move { BlockProducerRpcServer::new(mempool, store).serve(listener).await })
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
}

impl BlockProducerRpcServer {
    pub fn new(mempool: SharedMempool, store: StoreClient) -> Self {
        Self { mempool: Mutex::new(mempool), store }
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
        name = "block_producer:submit_proven_transaction",
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
        let tx = AuthenticatedTransaction::new(tx, inputs)?;

        self.mempool.lock().await.lock().await.add_transaction(tx).map(|block_height| {
            SubmitProvenTransactionResponse { block_height: block_height.as_u32() }
        })
    }
}
