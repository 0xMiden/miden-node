use std::{net::SocketAddr, time::Duration};

use futures::{TryStream, TryStreamExt};
use miden_node_proto::{
    domain::mempool::MempoolEvent,
    generated::{self as proto, block_producer::api_client::ApiClient},
};
use miden_node_utils::{FlattenResult, tracing::grpc::OtelInterceptor};
use miden_objects::{block::BlockNumber, transaction::ProvenTransaction};
use miden_tx::utils::Serializable;
use tokio_stream::StreamExt;
use tonic::{Status, service::interceptor::InterceptedService, transport::Channel};
use tracing::{info, instrument};

use crate::COMPONENT;

type InnerClient = ApiClient<InterceptedService<Channel, OtelInterceptor>>;

/// Interface to the block producer's gRPC API.
///
/// Essentially just a thin wrapper around the generated gRPC client which improves type safety.
#[derive(Clone, Debug)]
pub struct BlockProducerClient {
    inner: InnerClient,
}

impl BlockProducerClient {
    /// Creates a new block producer client with a lazy connection.
    pub fn new(block_producer_address: SocketAddr) -> Self {
        // SAFETY: The block_producer_url is always valid as it is created from a `SocketAddr`.
        let block_producer_url = format!("http://{block_producer_address}");
        let channel =
            tonic::transport::Endpoint::try_from(block_producer_url).unwrap().connect_lazy();
        let block_producer = ApiClient::with_interceptor(channel, OtelInterceptor);
        info!(target: COMPONENT, block_producer_endpoint = %block_producer_address, "Store client initialized");

        Self { inner: block_producer }
    }

    #[instrument(target = COMPONENT, name = "block_producer.client.submit_proven_transaction", skip_all, err)]
    pub async fn submit_proven_transaction(
        &self,
        proven_tx: ProvenTransaction,
    ) -> Result<(), Status> {
        let request = proto::transaction::ProvenTransaction { transaction: proven_tx.to_bytes() };

        self.inner.clone().submit_proven_transaction(request).await?;

        Ok(())
    }

    #[instrument(target = COMPONENT, name = "block_producer.client.subscribe_to_mempool", skip_all, err)]
    pub async fn subscribe_to_mempool_with_retry(
        &self,
        chain_tip: BlockNumber,
    ) -> Result<impl TryStream<Ok = MempoolEvent, Error = Status>, Status> {
        let mut retry_counter = 0;
        loop {
            match self.subscribe_to_mempool(chain_tip).await {
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
                result => return result,
            }
        }
    }

    async fn subscribe_to_mempool(
        &self,
        chain_tip: BlockNumber,
    ) -> Result<impl TryStream<Ok = MempoolEvent, Error = Status>, Status> {
        let request =
            proto::block_producer::MempoolSubscriptionRequest { chain_tip: chain_tip.as_u32() };
        let stream = self.inner.clone().mempool_subscription(request).await?;

        let stream = stream
            .into_inner()
            .map_ok(MempoolEvent::try_from)
            .map(FlattenResult::flatten_result);

        Ok(stream)
    }
}
