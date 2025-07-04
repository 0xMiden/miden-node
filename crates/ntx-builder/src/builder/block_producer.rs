use std::net::SocketAddr;

use miden_node_proto::generated::{
    block_producer::api_client::ApiClient, shared::SubmitProvenTransaction,
};
use miden_node_utils::tracing::grpc::OtelInterceptor;
use miden_objects::transaction::ProvenTransaction;
use miden_tx::utils::Serializable;
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
        let request = SubmitProvenTransaction { transaction: proven_tx.to_bytes() };

        self.inner.clone().submit_proven_transaction(request).await?;

        Ok(())
    }
}
