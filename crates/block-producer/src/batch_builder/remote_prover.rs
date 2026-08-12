use miden_node_proto::clients::{Builder, RemoteProverClient};
use miden_node_proto::domain::batch::decode_proven_batch;
use miden_node_proto::errors::ConversionError;
use miden_node_proto::generated::remote_prover::{ProofRequest, proof, proof_request};
use miden_node_utils::spawn::spawn_blocking_in_current_span;
use miden_protocol::MIN_PROOF_SECURITY_LEVEL;
use miden_protocol::batch::{ProposedBatch, ProvenBatch};
use miden_tx_batch::{BatchVerifier, LocalBatchProver};
use url::Url;

/// Errors returned by [`RemoteBatchProver`].
#[derive(Debug, thiserror::Error)]
pub enum RemoteProverError {
    #[error("remote prover request failed")]
    Grpc(#[source] tonic::Status),
    #[error("failed to decode proven batch from remote prover")]
    Decode(#[source] ConversionError),
    #[error("{0}")]
    Validation(String),
}

// BATCH PROVER
// ================================================================================================

/// Represents a batch prover which can be either local or remote.
#[derive(Clone)]
pub(super) enum BatchProver {
    Local(LocalBatchProver),
    Remote(Box<RemoteBatchProver>),
}

impl BatchProver {
    pub(super) const fn kind(&self) -> &'static str {
        match self {
            BatchProver::Local(_) => "local",
            BatchProver::Remote(_) => "remote",
        }
    }

    pub(super) fn local() -> Self {
        Self::Local(LocalBatchProver::new())
    }

    pub(super) fn remote(url: Url) -> anyhow::Result<Self> {
        Ok(Self::Remote(Box::new(RemoteBatchProver::new(url)?)))
    }
}

// REMOTE BATCH PROVER
// ================================================================================================

/// Thin wrapper around the remote-prover gRPC service that proves transaction batches.
///
/// The connection is lazy: the underlying channel connects on first use and is shared (cheaply
/// cloned) across all subsequent calls.
#[derive(Clone)]
pub(super) struct RemoteBatchProver {
    client: RemoteProverClient,
}

impl RemoteBatchProver {
    /// Creates a new [`RemoteBatchProver`] with a lazy connection to the given gRPC endpoint.
    fn new(url: Url) -> anyhow::Result<Self> {
        let client = Builder::new(url)
            .with_tls()?
            .without_timeout()
            .without_metadata_version()
            .without_metadata_genesis()
            .without_auth_header()
            .with_otel_context_injection()
            .connect_lazy::<RemoteProverClient>();

        Ok(Self { client })
    }

    pub(super) async fn prove(
        &self,
        proposed_batch: ProposedBatch,
    ) -> Result<ProvenBatch, RemoteProverError> {
        let request = tonic::Request::new(ProofRequest {
            request: Some(proof_request::Request::ProposedBatch((&proposed_batch).into())),
        });

        let response = self.client.clone().prove(request).await.map_err(RemoteProverError::Grpc)?;

        let batch = match response.into_inner().result {
            Some(proof::Result::ProvenBatch(batch)) => {
                decode_proven_batch(batch, &proposed_batch).map_err(RemoteProverError::Decode)
            },
            _ => Err(RemoteProverError::Validation(
                "remote batch prover returned the wrong proof kind".to_string(),
            )),
        }?;

        let batch_to_verify = batch.clone();
        spawn_blocking_in_current_span(move || {
            BatchVerifier::new(MIN_PROOF_SECURITY_LEVEL)
                .verify(&batch_to_verify)
                .map_err(|err| RemoteProverError::Validation(err.to_string()))
        })
        .await
        .map_err(|err| {
            RemoteProverError::Validation(format!("batch proof verification task failed: {err}"))
        })??;

        Ok(batch)
    }
}
