use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::Arc,
};
use core::time::Duration;

use miden_node_proto::clients::{ClientBuilder, RemoteClientManager};
use miden_objects::{
    transaction::{ProvenTransaction, TransactionWitness},
    utils::{Deserializable, DeserializationError, Serializable},
};
use miden_tx::{TransactionProver, TransactionProverError};

use super::generated::api_client::ApiClient;
use crate::{
    RemoteProverClientError,
    remote_prover::{
        generated,
        generated::{ProofType, ProvingRequest, ProvingResponse},
    },
};

// REMOTE TRANSACTION PROVER
// ================================================================================================

/// A [`RemoteTransactionProver`] is a transaction prover that sends witness data to a remote
/// gRPC server and receives a proven transaction.
///
/// When compiled for the `wasm32-unknown-unknown` target, it uses the `tonic_web_wasm_client`
/// transport. Otherwise, it uses the built-in `tonic::transport` for native platforms.
///
/// The transport layer connection is established lazily when the first transaction is proven.
#[derive(Clone)]
pub struct RemoteTransactionProver {
    #[cfg(target_arch = "wasm32")]
    manager: RemoteClientManager<ApiClient<tonic_web_wasm_client::Client>>,

    #[cfg(not(target_arch = "wasm32"))]
    manager: RemoteClientManager<ApiClient<tonic::transport::Channel>>,
}

impl RemoteTransactionProver {
    /// Creates a new [`RemoteTransactionProver`] with the specified gRPC server endpoint.
    ///
    /// The endpoint should be in the format `{protocol}://{hostname}:{port}`.
    /// Uses a default timeout of 10 seconds.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self::with_timeout(endpoint, None)
    }

    /// Creates a new [`RemoteTransactionProver`] with an optional custom timeout.
    ///
    /// If `timeout` is `None`, uses the default 10-second timeout.
    /// If `timeout` is `Some(duration)`, uses the specified timeout.
    pub fn with_timeout(endpoint: impl Into<String>, timeout: Option<Duration>) -> Self {
        RemoteTransactionProver {
            manager: RemoteClientManager::with_timeout(endpoint, timeout),
        }
    }

    /// Establishes a connection to the remote transaction prover server. The connection is
    /// maintained for the lifetime of the prover. If the connection is already established, this
    /// method does nothing.
    #[cfg(target_arch = "wasm32")]
    async fn connect(
        &self,
    ) -> Result<ApiClient<tonic_web_wasm_client::Client>, RemoteProverClientError> {
        self.manager
            .connect_with(|endpoint| async move {
                let web_client = tonic_web_wasm_client::Client::new(endpoint);
                Ok(ApiClient::new(web_client))
            })
            .await
    }

    /// Establishes a connection to the remote transaction prover server. The connection is
    /// maintained for the lifetime of the prover. If the connection is already established, this
    /// method does nothing.
    #[cfg(not(target_arch = "wasm32"))]
    async fn connect(
        &self,
    ) -> Result<ApiClient<tonic::transport::Channel>, RemoteProverClientError> {
        let timeout = self.manager.timeout();
        self.manager
            .connect_with(|endpoint| async move {
                let mut builder = ClientBuilder::new().with_tls();
                if let Some(timeout) = timeout {
                    builder = builder.with_timeout(timeout);
                }
                let channel = builder.create_channel(endpoint).await.map_err(|e| {
                    RemoteProverClientError::ConnectionFailed(
                        format!("Failed to create channel: {e}").into(),
                    )
                })?;
                Ok(ApiClient::new(channel))
            })
            .await
    }
}

#[async_trait::async_trait(?Send)]
impl TransactionProver for RemoteTransactionProver {
    async fn prove(
        &self,
        tx_witness: TransactionWitness,
    ) -> Result<ProvenTransaction, TransactionProverError> {
        use miden_objects::utils::Serializable;

        let mut client = self.connect().await.map_err(|err| {
            TransactionProverError::other_with_source("failed to connect to the remote prover", err)
        })?;

        let request = tonic::Request::new(tx_witness.into());

        let response = client.prove(request).await.map_err(|err| {
            TransactionProverError::other_with_source("failed to prove transaction", err)
        })?;

        // Deserialize the response bytes back into a ProvenTransaction.
        let proven_transaction =
            ProvenTransaction::try_from(response.into_inner()).map_err(|_| {
                TransactionProverError::other(
                    "failed to deserialize received response from remote transaction prover",
                )
            })?;

        Ok(proven_transaction)
    }
}

// CONVERSIONS
// ================================================================================================

impl TryFrom<ProvingResponse> for ProvenTransaction {
    type Error = DeserializationError;

    fn try_from(response: ProvingResponse) -> Result<Self, Self::Error> {
        ProvenTransaction::read_from_bytes(&response.payload)
    }
}

impl From<TransactionWitness> for ProvingRequest {
    fn from(witness: TransactionWitness) -> Self {
        ProvingRequest {
            proof_type: ProofType::Transaction.into(),
            payload: witness.to_bytes(),
        }
    }
}
