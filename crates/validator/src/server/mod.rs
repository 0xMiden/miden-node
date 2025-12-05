use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use miden_lib::block::build_block;
use miden_node_proto::generated::validator::api_server;
use miden_node_proto::generated::{self as proto};
use miden_node_proto_build::validator_api_descriptor;
use miden_node_utils::panic::catch_panic_layer_fn;
use miden_node_utils::tracing::grpc::grpc_trace_fn;
use miden_objects::block::ProposedBlock;
use miden_objects::ecdsa_signer::{EcdsaSigner, LocalEcdsaSigner};
use miden_objects::utils::{Deserializable, Serializable};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::trace::TraceLayer;

use crate::COMPONENT;

// VALIDATOR
// ================================================================================

/// The handle into running the gRPC validator server.
///
/// Facilitates the running of the gRPC server which implements the validator API.
pub struct Validator {
    /// The address of the validator component.
    pub address: SocketAddr,
    /// Server-side timeout for an individual gRPC request.
    ///
    /// If the handler takes longer than this duration, the server cancels the call.
    pub grpc_timeout: Duration,
}

impl Validator {
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
            .add_service(api_server::ApiServer::new(ValidatorServer {}))
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
struct ValidatorServer {}

#[tonic::async_trait]
impl api_server::Api for ValidatorServer {
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
        _request: tonic::Request<proto::transaction::ProvenTransaction>,
    ) -> Result<tonic::Response<()>, tonic::Status> {
        // TODO(sergerad): Implement transaction validation logic.
        Ok(tonic::Response::new(()))
    }

    /// Validates a proposed block and returns the block header and body.
    async fn sign_block(
        &self,
        request: tonic::Request<proto::blockchain::ProposedBlock>,
    ) -> Result<tonic::Response<proto::validator::Signature>, tonic::Status> {
        let proposed_block_bytes = request.into_inner().proposed_block;

        // Deserialize the proposed block.
        let proposed_block =
            ProposedBlock::read_from_bytes(&proposed_block_bytes).map_err(|err| {
                tonic::Status::invalid_argument(format!(
                    "Failed to deserialize proposed block: {err}",
                ))
            })?;

        // Build and sign header.
        let (header, _body) = build_block(proposed_block)
            .map_err(|err| tonic::Status::internal(format!("Failed to build block: {err}")))?;
        // TODO(sergerad): Inject a real signer into the validator.
        let signature = LocalEcdsaSigner::dummy().sign(header.commitment());

        // Send the signature.
        let response = proto::validator::Signature { signature: signature.to_bytes() };
        Ok(tonic::Response::new(response))
    }
}
