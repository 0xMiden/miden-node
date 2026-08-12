use std::time::Duration;

use miden_node_proto::clients::{Builder, RemoteProverClient};
use miden_node_proto::generated::remote_prover::{ProofRequest, proof, proof_request};
use miden_protocol::transaction::{ProvenTransaction, TransactionInputs};
use miden_protocol::utils::serde::Serializable;
use miden_tx::TransactionProverError;
use url::Url;

/// Thin wrapper around the remote-prover gRPC service that proves transactions.
///
/// The connection is lazy: the underlying channel connects on first use and is shared (cheaply
/// cloned) across all subsequent calls.
#[derive(Clone)]
pub struct RemoteTransactionProver {
    client: RemoteProverClient,
}

impl RemoteTransactionProver {
    /// Creates a new [`RemoteTransactionProver`] with a lazy connection to the given gRPC endpoint.
    pub fn new(url: Url, timeout: Duration) -> anyhow::Result<Self> {
        let client = Builder::new(url)
            .with_tls()?
            .with_timeout(timeout)
            .without_metadata_version()
            .without_metadata_genesis()
            .without_auth_header()
            .with_otel_context_injection()
            .connect_lazy::<RemoteProverClient>();

        Ok(Self { client })
    }

    pub async fn prove(
        &self,
        tx_inputs: &TransactionInputs,
    ) -> Result<ProvenTransaction, TransactionProverError> {
        let request = tonic::Request::new(ProofRequest {
            request: Some(proof_request::Request::TransactionInputs(tx_inputs.to_bytes())),
        });

        let response = self.client.clone().prove(request).await.map_err(|err| {
            TransactionProverError::other_with_source("failed to prove transaction", err)
        })?;

        match response.into_inner().result {
            Some(proof::Result::ProvenTransaction(transaction)) => {
                ProvenTransaction::try_from(transaction).map_err(|err| {
                    TransactionProverError::other_with_source(
                        "failed to decode received response from remote transaction prover",
                        err,
                    )
                })
            },
            _ => Err(TransactionProverError::other(
                "remote transaction prover returned the wrong proof kind",
            )),
        }
    }
}
