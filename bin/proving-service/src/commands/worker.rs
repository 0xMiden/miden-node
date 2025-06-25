use clap::Parser;
use miden_node_utils::cors::cors_for_grpc_web_layer;
use miden_proving_service::{
    COMPONENT,
    api::{ProofType, RpcListener},
    generated::api_server::ApiServer,
};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic_health::server::health_reporter;
use tonic_web::GrpcWebLayer;
use tracing::{info, instrument};

/// Starts a worker.
#[derive(Debug, Parser)]
pub struct StartWorker {
    /// Use localhost (127.0.0.1) instead of 0.0.0.0
    #[arg(long, env = "MPS_WORKER_LOCALHOST")]
    localhost: bool,
    /// The port of the worker
    #[arg(long, default_value = "50051", env = "MPS_WORKER_PORT")]
    port: u16,
    /// The type of proof that the worker will be handling
    #[arg(long, env = "MPS_WORKER_PROOF_TYPE")]
    proof_type: ProofType,
}

impl StartWorker {
    /// Starts a worker.
    ///
    /// This method receives the port from the CLI and starts a worker on that port.
    /// The host will be 127.0.0.1 if --localhost is specified, otherwise 0.0.0.0.
    /// In case that the port is not provided, it will default to `50051`.
    ///
    /// The worker includes a health reporter that will mark the service as serving, following the
    /// [gRPC health checking protocol](
    /// https://github.com/grpc/grpc-proto/blob/master/grpc/health/v1/health.proto).
    #[instrument(target = COMPONENT, name = "worker.execute")]
    pub async fn execute(&self) -> Result<(), String> {
        let host = if self.localhost { "127.0.0.1" } else { "0.0.0.0" };
        let worker_addr = format!("{}:{}", host, self.port);
        let rpc = RpcListener::new(
            TcpListener::bind(&worker_addr).await.map_err(|err| err.to_string())?,
            self.proof_type,
        );

        let server_addr = rpc.listener.local_addr().map_err(|err| err.to_string())?;
        info!(target: COMPONENT,
            endpoint = %server_addr,
            proof_type = ?self.proof_type,
            host = %host,
            port = %self.port,
            "Worker server initialized and listening"
        );

        // Create a health reporter
        let (health_reporter, health_service) = health_reporter();

        // Mark the service as serving
        health_reporter.set_serving::<ApiServer<RpcListener>>().await;

        tonic::transport::Server::builder()
            .accept_http1(true)
            .layer(cors_for_grpc_web_layer())
            .layer(GrpcWebLayer::new())
            .add_service(rpc.api_service)
            .add_service(rpc.status_service)
            .add_service(health_service)
            .serve_with_incoming(TcpListenerStream::new(rpc.listener))
            .await
            .map_err(|err| err.to_string())?;

        Ok(())
    }
}
