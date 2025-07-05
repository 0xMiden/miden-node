use std::net::SocketAddr;

use miden_node_proto::{
    clients::ClientBuilder, generated::requests::SubmitProvenTransactionRequest,
};
use miden_objects::transaction::ProvenTransaction;
use miden_tx::utils::Serializable;
use tonic::Status;
use tracing::{info, instrument};

use crate::COMPONENT;

type InnerClient = miden_node_proto::generated::block_producer::api_client::ApiClient<
    tonic::service::interceptor::InterceptedService<
        tonic::transport::Channel,
        miden_node_utils::tracing::grpc::OtelInterceptor,
    >,
>;

/// Interface to the block producer's gRPC API.
///
/// Essentially just a thin wrapper around the generated gRPC client which improves type safety.
#[derive(Clone, Debug)]
pub struct BlockProducerClient {
    inner: InnerClient,
}

impl BlockProducerClient {
    /// Creates a new block producer client with a lazy connection.
    pub async fn new(
        block_producer_address: SocketAddr,
    ) -> Result<Self, miden_node_proto::clients::ClientError> {
        let client = ClientBuilder::new()
            .with_otel()
            .with_lazy_connection(true)
            .build_block_producer_api_client(block_producer_address)
            .await?;

        info!(target: COMPONENT, block_producer_endpoint = %block_producer_address, "Block producer client initialized");

        Ok(Self { inner: client })
    }

    #[instrument(target = COMPONENT, name = "block_producer.client.submit_proven_transaction", skip_all, err)]
    pub async fn submit_proven_transaction(
        &self,
        proven_tx: ProvenTransaction,
    ) -> Result<(), Status> {
        let request = SubmitProvenTransactionRequest { transaction: proven_tx.to_bytes() };

        self.inner.clone().submit_proven_transaction(request).await?;

        Ok(())
    }
}
