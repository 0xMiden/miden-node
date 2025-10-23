use miden_node_proto::clients::{Builder, Rpc, RpcClient as InnerRpcClient};
use miden_node_proto::generated::{self as proto};
use miden_objects::transaction::{ProvenTransaction, TransactionInputs};
use miden_tx::utils::Serializable;
use tonic::Status;
use tracing::{info, instrument};
use url::Url;

use crate::COMPONENT;

// RPC CLIENT
// ================================================================================================

/// Interface to the RPC server's gRPC API for submitting transactions.
///
/// Essentially just a thin wrapper around the generated gRPC client which improves type safety.
/// This is required while the RPC forwards transactions to the validator as part of the network's
/// "guardrails". When those guardrails are removed, this client can be deleted and the network
/// transaction builder can forward transactions directly to the block producer instead of RPC.
#[derive(Clone, Debug)]
pub struct RpcClient {
    client: InnerRpcClient,
}

impl RpcClient {
    /// Creates a new RPC client with a lazy connection.
    pub fn new(rpc_url: Url) -> Self {
        info!(target: COMPONENT, rpc_endpoint = %rpc_url, "Initializing RPC client with lazy connection");

        let rpc_client = Builder::new(rpc_url)
            .without_tls()
            .without_timeout()
            .without_metadata_version()
            .without_metadata_genesis()
            .connect_lazy::<Rpc>();

        Self { client: rpc_client }
    }

    #[instrument(target = COMPONENT, name = "rpc.client.submit_proven_transaction", skip_all, err)]
    pub async fn submit_proven_transaction(
        &self,
        proven_tx: ProvenTransaction,
        transaction_inputs: TransactionInputs,
    ) -> Result<(), Status> {
        let request = proto::transaction::ProvenTransaction {
            transaction: proven_tx.to_bytes(),
            transaction_inputs: Some(transaction_inputs.to_bytes()),
        };

        self.client.clone().submit_proven_transaction(request).await?;

        Ok(())
    }
}
