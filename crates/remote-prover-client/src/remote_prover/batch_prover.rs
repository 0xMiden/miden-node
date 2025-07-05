use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::time::Duration;

use miden_node_proto::clients::{ClientBuilder, RemoteClientManager};
use miden_objects::{
    batch::{ProposedBatch, ProvenBatch},
    transaction::{OutputNote, ProvenTransaction, TransactionHeader, TransactionId},
    utils::{Deserializable, DeserializationError, Serializable},
};

use super::generated::api_client::ApiClient;
use crate::{
    RemoteProverClientError,
    remote_prover::generated::{ProofType, ProvingRequest, ProvingResponse},
};

// REMOTE BATCH PROVER
// ================================================================================================

/// A [`RemoteBatchProver`] is a batch prover that sends a proposed batch data to a remote
/// gRPC server and receives a proven batch.
///
/// When compiled for the `wasm32-unknown-unknown` target, it uses the `tonic_web_wasm_client`
/// transport. Otherwise, it uses the built-in `tonic::transport` for native platforms.
///
/// The transport layer connection is established lazily when the first batch is proven.
#[derive(Clone)]
pub struct RemoteBatchProver {
    #[cfg(target_arch = "wasm32")]
    manager: RemoteClientManager<ApiClient<tonic_web_wasm_client::Client>>,

    #[cfg(not(target_arch = "wasm32"))]
    manager: RemoteClientManager<ApiClient<tonic::transport::Channel>>,
}

impl RemoteBatchProver {
    /// Creates a new [`RemoteBatchProver`] with the specified gRPC server endpoint.
    ///
    /// The endpoint should be in the format `{protocol}://{hostname}:{port}`.
    /// Uses a default timeout of 10 seconds.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self::with_timeout(endpoint, None)
    }

    /// Creates a new [`RemoteBatchProver`] with an optional custom timeout.
    ///
    /// If `timeout` is `None`, uses the default 10-second timeout.
    /// If `timeout` is `Some(duration)`, uses the specified timeout.
    pub fn with_timeout(endpoint: impl Into<String>, timeout: Option<Duration>) -> Self {
        RemoteBatchProver {
            manager: RemoteClientManager::with_timeout(endpoint, timeout),
        }
    }

    /// Establishes a connection to the remote batch prover server. The connection is
    /// maintained for the lifetime of the prover. If the connection is already established, this
    /// method does nothing.
    #[cfg(target_arch = "wasm32")]
    async fn connect(
        &self,
    ) -> Result<ApiClient<tonic_web_wasm_client::Client>, RemoteProverClientError> {
        self.manager
            .connect_wasm_with(|endpoint| {
                let web_client = tonic_web_wasm_client::Client::new(endpoint);
                Ok(ApiClient::new(web_client))
            })
            .await
    }

    /// Establishes a connection to the remote batch prover server. The connection is
    /// maintained for the lifetime of the prover. If the connection is already established, this
    /// method does nothing.
    #[cfg(not(target_arch = "wasm32"))]
    async fn connect(
        &self,
    ) -> Result<ApiClient<tonic::transport::Channel>, RemoteProverClientError> {
        self.manager
            .connect_with(|endpoint, builder| async move {
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

impl RemoteBatchProver {
    pub async fn prove(
        &self,
        proposed_batch: ProposedBatch,
    ) -> Result<ProvenBatch, RemoteProverClientError> {
        use miden_objects::utils::Serializable;

        let mut client = self.connect().await?;

        // Create the set of the transactions we pass to the prover for later validation.
        let proposed_txs: Vec<_> = proposed_batch.transactions().iter().map(Arc::clone).collect();

        let request = tonic::Request::new(proposed_batch.into());

        let response = client.prove(request).await.map_err(|err| {
            RemoteProverClientError::other_with_source("failed to prove block", err)
        })?;

        // Deserialize the response bytes back into a ProvenBatch.
        let proven_batch = ProvenBatch::try_from(response.into_inner()).map_err(|err| {
            RemoteProverClientError::other_with_source(
                "failed to deserialize received response from remote prover",
                err,
            )
        })?;

        Self::validate_tx_headers(&proven_batch, proposed_txs)?;

        Ok(proven_batch)
    }

    /// Validates that the proven batch's transaction headers are consistent with the transactions
    /// passed in the proposed batch.
    ///
    /// Note that we expect all input and output notes from a proposed transaction to be present
    /// in the corresponding header as well, because note erasure doesn't matter for the transaction
    /// itself and we want the original transaction data to be preserved.
    ///
    /// This expects that proposed transactions and batch transactions are in the same order, as
    /// define by `OrderedTransactionHeaders`.
    fn validate_tx_headers(
        proven_batch: &ProvenBatch,
        proposed_txs: Vec<Arc<ProvenTransaction>>,
    ) -> Result<(), RemoteProverClientError> {
        if proposed_txs.len() != proven_batch.transactions().as_slice().len() {
            return Err(RemoteProverClientError::other(format!(
                "remote prover returned {} transaction headers but {} transactions were passed as part of the proposed batch",
                proven_batch.transactions().as_slice().len(),
                proposed_txs.len()
            )));
        }

        // Because we checked the length matches we can zip the iterators up.
        // We expect the transactions to be in the same order.
        for (proposed_header, proven_header) in
            proposed_txs.into_iter().zip(proven_batch.transactions().as_slice())
        {
            if proven_header.account_id() != proposed_header.account_id() {
                return Err(RemoteProverClientError::other(format!(
                    "transaction header of {} has a different account ID than the proposed transaction",
                    proposed_header.id()
                )));
            }

            if proven_header.initial_state_commitment()
                != proposed_header.account_update().initial_state_commitment()
            {
                return Err(RemoteProverClientError::other(format!(
                    "transaction header of {} has a different initial state commitment than the proposed transaction",
                    proposed_header.id()
                )));
            }

            if proven_header.final_state_commitment()
                != proposed_header.account_update().final_state_commitment()
            {
                return Err(RemoteProverClientError::other(format!(
                    "transaction header of {} has a different final state commitment than the proposed transaction",
                    proposed_header.id()
                )));
            }

            // Check input notes
            let num_notes = proposed_header.input_notes().num_notes() as usize;
            if num_notes != proven_header.input_notes().len() {
                return Err(RemoteProverClientError::other(format!(
                    "transaction header of {} has a different number of input notes than the proposed transaction",
                    proposed_header.id()
                )));
            }

            // Because we checked the length matches we can zip the iterators up.
            // We expect the nullifiers to be in the same order.
            for (proposed_nullifier, header_nullifier) in
                proposed_header.nullifiers().zip(proven_header.input_notes().iter())
            {
                if proposed_nullifier != *header_nullifier {
                    return Err(RemoteProverClientError::other(format!(
                        "transaction header of {} has a different set of input notes than the proposed transaction",
                        proposed_header.id()
                    )));
                }
            }

            // Check output notes
            if proposed_header.output_notes().num_notes() != proven_header.output_notes().len() {
                return Err(RemoteProverClientError::other(format!(
                    "transaction header of {} has a different number of output notes than the proposed transaction",
                    proposed_header.id()
                )));
            }

            // Because we checked the length matches we can zip the iterators up.
            // We expect the note IDs to be in the same order.
            for (proposed_note_id, header_note_id) in proposed_header
                .output_notes()
                .iter()
                .map(OutputNote::id)
                .zip(proven_header.output_notes().iter())
            {
                if proposed_note_id != *header_note_id {
                    return Err(RemoteProverClientError::other(format!(
                        "transaction header of {} has a different set of input notes than the proposed transaction",
                        proposed_header.id()
                    )));
                }
            }
        }

        Ok(())
    }
}

// CONVERSIONS
// ================================================================================================

impl From<ProposedBatch> for ProvingRequest {
    fn from(proposed_batch: ProposedBatch) -> Self {
        ProvingRequest {
            proof_type: ProofType::Batch.into(),
            payload: proposed_batch.to_bytes(),
        }
    }
}

impl TryFrom<ProvingResponse> for ProvenBatch {
    type Error = DeserializationError;

    fn try_from(response: ProvingResponse) -> Result<Self, Self::Error> {
        ProvenBatch::read_from_bytes(&response.payload)
    }
}
