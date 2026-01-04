use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use miden_node_proto::generated::validator::api_server;
use miden_node_proto::generated::{self as proto};
use miden_node_proto_build::validator_api_descriptor;
use miden_node_utils::ErrorReport;
use miden_node_utils::panic::catch_panic_layer_fn;
use miden_node_utils::tracing::grpc::grpc_trace_fn;
use miden_protocol::block::{BlockSigner, ProposedBlock};
use miden_protocol::transaction::{
    ProvenTransaction,
    TransactionHeader,
    TransactionId,
    TransactionInputs,
};
use miden_tx::utils::{Deserializable, Serializable};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::Status;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::trace::TraceLayer;

use crate::COMPONENT;
use crate::block_validation::validate_block;
use crate::tx_validation::validate_transaction;

/// A type alias for a read-write lock that stores validated transactions.
pub type ValidatedTransactions = RwLock<HashMap<TransactionId, TransactionHeader>>;

// VALIDATOR
// ================================================================================

/// The handle into running the gRPC validator server.
///
/// Facilitates the running of the gRPC server which implements the validator API.
pub struct Validator<S> {
    /// The address of the validator component.
    pub address: SocketAddr,
    /// Server-side timeout for an individual gRPC request.
    ///
    /// If the handler takes longer than this duration, the server cancels the call.
    pub grpc_timeout: Duration,

    /// The signer used to sign blocks.
    pub signer: S,
}

impl<S: BlockSigner + Send + Sync + 'static> Validator<S> {
    /// Serves the validator RPC API.
    ///
    /// Executes in place (i.e. not spawned) and will run indefinitely until a fatal error is
    /// encountered.
    pub async fn serve(self) -> anyhow::Result<()> {
        tracing::info!(target: COMPONENT, endpoint=?self.address, "Initializing server");

        let listener = TcpListener::bind(self.address)
            .await
            .context("failed to bind to block producer address")?;

        let reflection_service = tonic_reflection::server::Builder::configure()
            .register_file_descriptor_set(validator_api_descriptor())
            .build_v1()
            .context("failed to build reflection service")?;

        // This is currently required for postman to work properly because
        // it doesn't support the new version yet.
        //
        // See: <https://github.com/postmanlabs/postman-app-support/issues/13120>.
        let reflection_service_alpha = tonic_reflection::server::Builder::configure()
            .register_file_descriptor_set(validator_api_descriptor())
            .build_v1alpha()
            .context("failed to build reflection service")?;

        // Build the gRPC server with the API service and trace layer.
        tonic::transport::Server::builder()
            .layer(CatchPanicLayer::custom(catch_panic_layer_fn))
            .layer(TraceLayer::new_for_grpc().make_span_with(grpc_trace_fn))
            .timeout(self.grpc_timeout)
            .add_service(api_server::ApiServer::new(ValidatorServer::new(self.signer)))
            .add_service(reflection_service)
            .add_service(reflection_service_alpha)
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .context("failed to serve validator API")
    }
}

// VALIDATOR SERVER
// ================================================================================

/// The underlying implementation of the gRPC validator server.
///
/// Implements the gRPC API for the validator.
struct ValidatorServer<S> {
    signer: S,
    validated_transactions: Arc<ValidatedTransactions>,
}

impl<S> ValidatorServer<S> {
    fn new(signer: S) -> Self {
        let validated_transactions = Arc::new(ValidatedTransactions::default());
        Self { signer, validated_transactions }
    }
}

#[tonic::async_trait]
impl<S: BlockSigner + Send + Sync + 'static> api_server::Api for ValidatorServer<S> {
    /// Returns the status of the validator.
    async fn status(
        &self,
        _request: tonic::Request<()>,
    ) -> Result<tonic::Response<proto::validator::ValidatorStatus>, tonic::Status> {
        Ok(tonic::Response::new(proto::validator::ValidatorStatus {
            version: env!("CARGO_PKG_VERSION").to_string(),
            status: "OK".to_string(),
        }))
    }

    /// Receives a proven transaction, then validates and stores it.
    async fn submit_proven_transaction(
        &self,
        request: tonic::Request<proto::transaction::ProvenTransaction>,
    ) -> Result<tonic::Response<()>, tonic::Status> {
        let request = request.into_inner();
        // Deserialize the transaction.
        let proven_tx =
            ProvenTransaction::read_from_bytes(&request.transaction).map_err(|err| {
                Status::invalid_argument(err.as_report_context("Invalid proven transaction"))
            })?;

        // Deserialize the transaction inputs.
        let Some(tx_inputs) = request.transaction_inputs else {
            return Err(Status::invalid_argument("Missing transaction inputs"));
        };
        let tx_inputs = TransactionInputs::read_from_bytes(&tx_inputs).map_err(|err| {
            Status::invalid_argument(err.as_report_context("Invalid transaction inputs"))
        })?;

        // Validate the transaction.
        let validated_tx_header =
            validate_transaction(proven_tx, tx_inputs).await.map_err(|err| {
                Status::invalid_argument(err.as_report_context("Invalid transaction"))
            })?;

        // Register the validated transaction.
        let tx_id = validated_tx_header.id();
        self.validated_transactions.write().await.insert(tx_id, validated_tx_header);

        Ok(tonic::Response::new(()))
    }

    /// Validates a proposed block and returns the block header and body.
    async fn sign_block(
        &self,
        request: tonic::Request<proto::blockchain::ProposedBlock>,
    ) -> Result<tonic::Response<proto::blockchain::BlockSignature>, tonic::Status> {
        let proposed_block_bytes = request.into_inner().proposed_block;

        // Deserialize the proposed block.
        let proposed_block =
            ProposedBlock::read_from_bytes(&proposed_block_bytes).map_err(|err| {
                tonic::Status::invalid_argument(format!(
                    "Failed to deserialize proposed block: {err}",
                ))
            })?;

        // Validate the block.
        let signature =
            validate_block(proposed_block, &self.signer, self.validated_transactions.clone())
                .await
                .map_err(|err| {
                    tonic::Status::invalid_argument(format!("Failed to validate block: {err}",))
                })?;

        // Send the signature.
        let response = proto::blockchain::BlockSignature { signature: signature.to_bytes() };
        Ok(tonic::Response::new(response))
    }
}
