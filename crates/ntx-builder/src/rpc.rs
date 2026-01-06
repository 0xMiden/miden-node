use miden_node_proto::clients::{Builder, RpcClient as InnerRpcClient};
use miden_node_proto::generated::{self as proto};
use miden_protocol::transaction::{ProvenTransaction, TransactionInputs};
use miden_tx::utils::Serializable;
use tonic::Status;
use tracing::{info, instrument};
use url::Url;

use crate::COMPONENT;

// RPC CLIENT
// ================================================================================================

/// Interface to the RPC server's gRPC API.
#[derive(Clone, Debug)]
pub struct RpcClient {
    client: InnerRpcClient,
}

impl RpcClient {
    /// Creates a new RPC client with a lazy connection.
    pub fn new(rpc_url: Url) -> Self {
        info!(target: COMPONENT, rpc_endpoint = %rpc_url, "Initializing RPC client with lazy connection");

        let rpc = Builder::new(rpc_url)
            .without_tls()
            .without_timeout()
            .without_metadata_version()
            .without_metadata_genesis()
            .with_otel_context_injection()
            .connect_lazy::<InnerRpcClient>();

        Self { client: rpc }
    }

    /// Submits a proven transaction with transaction inputs to the RPC server.
    #[instrument(target = COMPONENT, name = "ntx.rpc.client.submit_proven_transaction", skip_all, err)]
    pub async fn submit_proven_transaction(
        &self,
        proven_tx: ProvenTransaction,
        tx_inputs: TransactionInputs,
    ) -> Result<(), Status> {
        let request = proto::transaction::ProvenTransaction {
            transaction: proven_tx.to_bytes(),
            transaction_inputs: Some(tx_inputs.to_bytes()),
        };

        self.client.clone().submit_proven_transaction(request).await?;

        Ok(())
    }
}
