use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use pingora::{server::ListenFds, services::Service};
use tokio::{net::TcpListener, sync::watch, time::interval};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, Response, Status, transport::Server};
use tracing::{error, info};

use super::worker::WorkerHealthStatus as RustWorkerHealthStatus;
use crate::{
    commands::PROXY_HOST,
    generated::{
        proving_service::ProofType,
        proxy_status::{
            ProxyStatusRequest, ProxyStatusResponse, WorkerHealthStatus, WorkerStatus,
            proxy_status_api_server::{ProxyStatusApi, ProxyStatusApiServer},
        },
    },
    proxy::LoadBalancerState,
};

/// Update cache every 5 seconds
const CACHE_UPDATE_INTERVAL_SECS: u64 = 5;

/// Cached proxy status response
type CachedStatus = ProxyStatusResponse;

// PROXY STATUS SERVICE
// ================================================================================================

/// gRPC service that implements Pingora's Service trait
pub struct ProxyStatusService {
    load_balancer: Arc<LoadBalancerState>,
    port: u16,
    status_tx: watch::Sender<CachedStatus>,
    update_interval: Duration,
}

/// Internal gRPC service implementation
pub struct ProxyStatusGrpcService {
    status_rx: watch::Receiver<CachedStatus>,
}

impl ProxyStatusService {
    pub async fn new(load_balancer: Arc<LoadBalancerState>, port: u16) -> Self {
        // Create initial status
        let initial_status = generate_status(&load_balancer).await;

        let (status_tx, _) = watch::channel(initial_status);
        let update_interval = Duration::from_secs(CACHE_UPDATE_INTERVAL_SECS);

        Self {
            load_balancer,
            port,
            status_tx,
            update_interval,
        }
    }
}

/// Generates a new status and stores it in the cache.
async fn generate_and_store_status(
    load_balancer: &LoadBalancerState,
    status_tx: &watch::Sender<CachedStatus>,
) {
    let status = generate_status(load_balancer).await;

    // This will notify all receivers of the new status
    let _ = status_tx.send(status);
}

/// Generates a new status from the load balancer.
async fn generate_status(load_balancer: &LoadBalancerState) -> ProxyStatusResponse {
    let workers = load_balancer.workers.read().await;
    let worker_statuses: Vec<WorkerStatus> = workers
        .iter()
        .map(|w| WorkerStatus {
            address: w.address(),
            version: w.version().to_string(),
            status: WorkerHealthStatus::from(w.health_status()) as i32,
        })
        .collect();

    let supported_proof_type: ProofType = load_balancer.supported_prover_type.into();

    ProxyStatusResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        supported_proof_type: supported_proof_type as i32,
        workers: worker_statuses,
    }
}

/// gRPC service that implements Pingora's Service trait
impl ProxyStatusGrpcService {
    fn new(status_rx: watch::Receiver<CachedStatus>) -> Self {
        Self { status_rx }
    }
}

#[async_trait]
impl ProxyStatusApi for ProxyStatusGrpcService {
    async fn status(
        &self,
        _request: Request<ProxyStatusRequest>,
    ) -> Result<Response<ProxyStatusResponse>, Status> {
        // Get the latest status, or wait for it if it hasn't been set yet
        let status = self.status_rx.borrow().clone();
        Ok(Response::new(status))
    }
}

#[async_trait]
impl Service for ProxyStatusService {
    async fn start_service(
        &mut self,
        #[cfg(unix)] _fds: Option<ListenFds>,
        mut shutdown: watch::Receiver<bool>,
        _listeners_per_fd: usize,
    ) {
        info!("Starting gRPC status service on port {}", self.port);

        // Create a new listener
        let addr = format!("{}:{}", PROXY_HOST, self.port);
        let listener = match TcpListener::bind(&addr).await {
            Ok(listener) => {
                info!("gRPC status service bound to {}", addr);
                listener
            },
            Err(e) => {
                error!("Failed to bind gRPC status service to {}: {}", addr, e);
                return;
            },
        };

        // Generate initial status
        generate_and_store_status(&self.load_balancer, &self.status_tx).await;
        info!("Initial status populated for gRPC status service");

        // Start the status updater task
        let load_balancer = self.load_balancer.clone();
        let status_tx = self.status_tx.clone();
        let update_interval = self.update_interval;
        let mut cache_updater_shutdown = shutdown.clone();

        tokio::spawn(async move {
            let mut update_timer = interval(update_interval);

            loop {
                tokio::select! {
                    _ = update_timer.tick() => {
                        generate_and_store_status(&load_balancer, &status_tx).await;
                    }
                    _ = cache_updater_shutdown.changed() => {
                        info!("Status updater received shutdown signal");
                        break;
                    }
                }
            }
        });

        // Create the gRPC service implementation with a new receiver
        let grpc_service = ProxyStatusGrpcService::new(self.status_tx.subscribe());
        let status_server = ProxyStatusApiServer::new(grpc_service);

        // Build the tonic server
        let server = Server::builder().add_service(status_server).serve_with_incoming_shutdown(
            TcpListenerStream::new(listener),
            async move {
                let _ = shutdown.changed().await;
                info!("gRPC status service received shutdown signal");
            },
        );

        // Run the server
        if let Err(e) = server.await {
            error!(err=?e, "gRPC status service failed");
        } else {
            info!("gRPC status service stopped gracefully");
        }
    }

    fn name(&self) -> &'static str {
        "grpc-status"
    }

    fn threads(&self) -> Option<usize> {
        Some(1) // Single thread is sufficient for the status service
    }
}

impl From<&RustWorkerHealthStatus> for WorkerHealthStatus {
    fn from(status: &RustWorkerHealthStatus) -> Self {
        match status {
            RustWorkerHealthStatus::Healthy => WorkerHealthStatus::Healthy,
            RustWorkerHealthStatus::Unhealthy { .. } => WorkerHealthStatus::Unhealthy,
            RustWorkerHealthStatus::Unknown => WorkerHealthStatus::Unknown,
        }
    }
}
