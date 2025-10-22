use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use miden_node_proto::generated::validator::api_server;
use miden_node_proto::generated::{self as proto};
use miden_node_proto_build::validator_api_descriptor;
use miden_node_utils::ErrorReport;
use miden_node_utils::panic::catch_panic_layer_fn;
use miden_node_utils::tracing::grpc::grpc_trace_fn;
use miden_objects::block::ProvenBlock;
use miden_objects::transaction::ProvenTransaction;
use miden_objects::utils::Deserializable;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::trace::TraceLayer;

use crate::COMPONENT;
use crate::db::load;
use crate::server::submit::submit_proven_transaction;
use crate::server::validate::{PrivateKey, validate_block};

mod submit;
mod validate;

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
    /// The data directory for the validator component's database files.
    pub data_directory: PathBuf,
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

        // Initialize database connection.
        let db = load(self.data_directory.join("validator.sqlite3"))
            .await
            .context("failed to initialize validator database")?;

        // Build the gRPC server with the API service and trace layer.
        let validator_server = ValidatorServer::new(db);
        tonic::transport::Server::builder()
            .layer(CatchPanicLayer::custom(catch_panic_layer_fn))
            .layer(TraceLayer::new_for_grpc().make_span_with(grpc_trace_fn))
            .timeout(self.grpc_timeout)
            .add_service(api_server::ApiServer::new(validator_server))
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
struct ValidatorServer {
    pub db: miden_node_store::Db,
}

impl ValidatorServer {
    pub fn new(db: miden_node_store::Db) -> Self {
        Self { db }
    }
}

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
        request: tonic::Request<proto::transaction::ProvenTransaction>,
    ) -> Result<tonic::Response<()>, tonic::Status> {
        let proto_transaction = request.into_inner();

        // Deserialize the proven transaction from protobuf bytes.
        let proven_tx = ProvenTransaction::read_from_bytes(&proto_transaction.transaction)
            .map_err(|err| {
                tonic::Status::invalid_argument(
                    format!("failed to deserialize transaction: {err}",),
                )
            })?;

        let result = self
            .db
            .transact("submit_proven_transaction", move |conn| {
                submit_proven_transaction(conn, &proven_tx)
            })
            .await;

        match result {
            Ok(rows_affected) => {
                if rows_affected == 1 {
                    Ok(tonic::Response::new(()))
                } else {
                    tracing::error!(
                        target: COMPONENT,
                        rows_affected = rows_affected,
                        "unexpected number of rows affected by insertion of proven transaction"
                    );
                    Err(tonic::Status::internal("failed to submit proven transaction"))
                }
            },
            Err(err) => Err(err.into()),
        }
    }

    /// Receives a proven block and validates it.
    async fn validate_block(
        &self,
        request: tonic::Request<proto::blockchain::Block>,
    ) -> std::result::Result<tonic::Response<proto::validator::SignedBlock>, tonic::Status> {
        let request = request.into_inner();
        let proven_block = ProvenBlock::read_from_bytes(&request.block).map_err(|err| {
            tonic::Status::invalid_argument(err.as_report_context("block deserialization error"))
        })?;

        let signed_block = validate_block(proven_block, PrivateKey {}).unwrap();
        Ok(tonic::Response::new(()))
    }
}
