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

/// Update status every 5 seconds
const STATUS_UPDATE_INTERVAL_SECS: u64 = 5;

// PROXY STATUS SERVICE
// ================================================================================================

/// The gRPC service that implements Pingora's Service trait and the gRPC API.
///
/// The service is responsible for serving the gRPC status API for the proxy.
///
/// Implements the [`Service`] trait and the [`ProxyStatusApi`] gRPC API.
#[derive(Clone)]
pub struct ProxyStatusPingoraService {
    /// The load balancer state.
    ///
    /// This is used to generate the status response.
    load_balancer: Arc<LoadBalancerState>,
    /// The port to serve the gRPC status API on.
    port: u16,
    /// The status receiver.
    ///
    /// This is used to receive the status updates from the updater.
    status_rx: watch::Receiver<ProxyStatusResponse>,
    /// The status transmitter.
    ///
    /// This is used to send the status updates to the receiver.
    status_tx: watch::Sender<ProxyStatusResponse>,
}

impl ProxyStatusPingoraService {
    /// Creates a new [`ProxyStatusPingoraService`].
    pub async fn new(load_balancer: Arc<LoadBalancerState>, port: u16) -> Self {
        // Generate initial status
        let initial_status = build_status(&load_balancer).await;
        let (status_tx, status_rx) = watch::channel(initial_status);

        Self {
            load_balancer,
            port,
            status_rx,
            status_tx,
        }
    }
}

#[async_trait]
impl ProxyStatusApi for ProxyStatusPingoraService {
    /// Returns the current status of the proxy.
    async fn status(
        &self,
        _request: Request<ProxyStatusRequest>,
    ) -> Result<Response<ProxyStatusResponse>, Status> {
        // Get the latest status, or wait for it if it hasn't been set yet
        let status = self.status_rx.borrow().clone();
        Ok(Response::new(status))
    }
}

/// The [`Service`] trait implementation for the proxy status service.
///
/// This is used to start the service and handle the shutdown signal.
#[async_trait]
impl Service for ProxyStatusPingoraService {
    async fn start_service(
        &mut self,
        #[cfg(unix)] _fds: Option<ListenFds>,
        shutdown: watch::Receiver<bool>,
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

        // Start the status updater task
        let updater = ProxyStatusUpdater::new(self.load_balancer.clone(), self.status_tx.clone());
        let cache_updater_shutdown = shutdown.clone();
        let updater_task = async move {
            updater.start(cache_updater_shutdown).await;
        };

        // Build the tonic server with self as the gRPC API implementation
        let status_server = ProxyStatusApiServer::new(self.clone());
        let mut server_shutdown = shutdown.clone();
        let server = Server::builder().add_service(status_server).serve_with_incoming_shutdown(
            TcpListenerStream::new(listener),
            async move {
                let _ = server_shutdown.changed().await;
                info!("gRPC status service received shutdown signal");
            },
        );

        // Run both the server and updater concurrently, if either fails, the whole service stops
        tokio::select! {
            result = server => {
                if let Err(e) = result {
                    error!(err=?e, "gRPC status service failed");
                } else {
                    info!("gRPC status service stopped gracefully");
                }
            }
            _ = updater_task => {
                error!("Status updater task ended unexpectedly");
            }
        }
    }

    fn name(&self) -> &'static str {
        "grpc-status"
    }

    fn threads(&self) -> Option<usize> {
        Some(1) // Single thread is sufficient for the status service
    }
}

// PROXY STATUS UPDATER
// ================================================================================================

/// The updater for the proxy status.
///
/// This is responsible for periodically updating the status of the proxy.
pub struct ProxyStatusUpdater {
    /// The load balancer state.
    ///
    /// This is used to generate the status response.
    load_balancer: Arc<LoadBalancerState>,
    /// The status transmitter.
    ///
    /// This is used to send the status updates to the proxy status service.
    status_tx: watch::Sender<ProxyStatusResponse>,
    /// The interval at which to update the status.
    update_interval: Duration,
}

impl ProxyStatusUpdater {
    /// Creates a new [`ProxyStatusUpdater`].
    pub fn new(
        load_balancer: Arc<LoadBalancerState>,
        status_tx: watch::Sender<ProxyStatusResponse>,
    ) -> Self {
        Self {
            load_balancer,
            status_tx,
            update_interval: Duration::from_secs(STATUS_UPDATE_INTERVAL_SECS),
        }
    }

    /// Starts the status updater.
    ///
    /// This is responsible for periodically updating the status of the proxy.
    pub async fn start(&self, mut shutdown: watch::Receiver<bool>) {
        let mut update_timer = interval(self.update_interval);
        loop {
            tokio::select! {
                _ = update_timer.tick() => {
                    let new_status = build_status(&self.load_balancer).await;
                    let _ = self.status_tx.send(new_status);
                }
                _ = shutdown.changed() => {
                    info!("Status updater received shutdown signal");
                    break;
                }
            }
        }
    }
}

// UTILS
// ================================================================================================

impl From<&RustWorkerHealthStatus> for WorkerHealthStatus {
    fn from(status: &RustWorkerHealthStatus) -> Self {
        match status {
            RustWorkerHealthStatus::Healthy => WorkerHealthStatus::Healthy,
            RustWorkerHealthStatus::Unhealthy { .. } => WorkerHealthStatus::Unhealthy,
            RustWorkerHealthStatus::Unknown => WorkerHealthStatus::Unknown,
        }
    }
}

/// Build a new status from the load balancer and returns it as a [`ProxyStatusResponse`].
async fn build_status(load_balancer: &LoadBalancerState) -> ProxyStatusResponse {
    let workers = load_balancer.workers.read().await;
    let worker_statuses: Vec<WorkerStatus> = workers.iter().map(WorkerStatus::from).collect();

    let supported_proof_type: ProofType = load_balancer.supported_prover_type.into();

    ProxyStatusResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        supported_proof_type: supported_proof_type.into(),
        workers: worker_statuses,
    }
}
