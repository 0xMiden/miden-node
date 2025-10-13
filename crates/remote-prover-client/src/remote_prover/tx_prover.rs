use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use core::time::Duration;

use miden_objects::transaction::{ExecutedTransaction, ProvenTransaction};
use miden_objects::utils::{Deserializable, DeserializationError, Serializable};
use miden_objects::vm::FutureMaybeSend;
use miden_tx::TransactionProverError;
use tokio::sync::Mutex;

use super::generated::api_client::ApiClient;
use crate::RemoteProverClientError;
use crate::remote_prover::generated as proto;

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
    client: Arc<Mutex<Option<ApiClient<tonic_web_wasm_client::Client>>>>,

    #[cfg(not(target_arch = "wasm32"))]
    client: Arc<Mutex<Option<ApiClient<tonic::transport::Channel>>>>,

    endpoint: String,
}

impl RemoteTransactionProver {
    /// Creates a new [`RemoteTransactionProver`] with the specified gRPC server endpoint. The
    /// endpoint should be in the format `{protocol}://{hostname}:{port}`.
    pub fn new(endpoint: impl Into<String>) -> Self {
        RemoteTransactionProver {
            endpoint: endpoint.into(),
            client: Arc::new(Mutex::new(None)),
        }
    }

    /// Establishes a connection to the remote transaction prover server. The connection is
    /// maintained for the lifetime of the prover. If the connection is already established, this
    /// method does nothing.
    async fn connect(&self) -> Result<(), RemoteProverClientError> {
        let mut client = self.client.lock().await;
        if client.is_some() {
            return Ok(());
        }

        #[cfg(target_arch = "wasm32")]
        let new_client = {
            let web_client = tonic_web_wasm_client::Client::new(self.endpoint.clone());
            ApiClient::new(web_client)
        };

        #[cfg(not(target_arch = "wasm32"))]
        let new_client = {
            let endpoint = tonic::transport::Endpoint::try_from(self.endpoint.clone())
                .map_err(|err| RemoteProverClientError::ConnectionFailed(err.into()))?
                .timeout(Duration::from_millis(10000));
            let channel = endpoint
                .tls_config(tonic::transport::ClientTlsConfig::new().with_native_roots())
                .map_err(|err| RemoteProverClientError::ConnectionFailed(err.into()))?
                .connect()
                .await
                .map_err(|err| RemoteProverClientError::ConnectionFailed(err.into()))?;
            ApiClient::new(channel)
        };

        *client = Some(new_client);

        Ok(())
    }
}

impl RemoteTransactionProver {
    pub fn prove(
        &self,
        executed_transaction: ExecutedTransaction,
    ) -> impl FutureMaybeSend<Result<ProvenTransaction, TransactionProverError>> {
        async move {
            use miden_objects::utils::Serializable;
            self.connect().await.map_err(|err| {
                TransactionProverError::other_with_source(
                    "failed to connect to the remote prover",
                    err,
                )
            })?;

            let mut client = self
                .client
                .lock()
                .await
                .as_ref()
                .ok_or_else(|| TransactionProverError::other("client should be connected"))?
                .clone();

            let request = tonic::Request::new(executed_transaction.into());

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
}

// CONVERSIONS
// ================================================================================================

impl TryFrom<proto::Proof> for ProvenTransaction {
    type Error = DeserializationError;

    fn try_from(response: proto::Proof) -> Result<Self, Self::Error> {
        ProvenTransaction::read_from_bytes(&response.payload)
    }
}

impl From<ExecutedTransaction> for proto::ProofRequest {
    fn from(tx: ExecutedTransaction) -> Self {
        proto::ProofRequest {
            proof_type: proto::ProofType::Transaction.into(),
            payload: tx.to_bytes(),
        }
    }
}
