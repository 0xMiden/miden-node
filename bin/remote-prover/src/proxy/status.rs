use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use miden_remote_prover::{
    api::ProofType,
    generated::remote_prover::{
        self as proto, ProxyStatusRequest, ProxyStatusResponse, WorkerStatus,
        proxy_status_api_server::{ProxyStatusApi, ProxyStatusApiServer},
    },
};
use pingora::{server::ListenFds, services::Service};
use tokio::{net::TcpListener, sync::watch, time::interval};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, Response, Status, transport::Server};
use tracing::{error, info, instrument};

use super::worker::WorkerHealthStatus;
use crate::{
    COMPONENT,
    commands::PROXY_HOST,
    proxy::{LoadBalancerState, worker::Worker},
};

// PROXY STATUS SERVICE
// ================================================================================================

/// The gRPC service that implements Pingora's Service trait and the gRPC API.
///
/// The service is responsible for serving the gRPC status API for the proxy.
///
/// Implements the [`Service`] trait and the [`ProxyStatusApi`] gRPC API.
#[derive(Clone, Debug)]
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
    /// The status update interval.
    status_update_interval: Duration,
}

impl ProxyStatusPingoraService {
    /// Creates a new [`ProxyStatusPingoraService`].
    pub async fn new(
        load_balancer: Arc<LoadBalancerState>,
        port: u16,
        status_update_interval: Duration,
    ) -> Self {
        let version = env!("CARGO_PKG_VERSION").to_string();
        let supported_proof_type: ProofType = load_balancer.supported_proof_type;
        let supported_proof_type: i32 = supported_proof_type.into();

        let initial_status = {
            // Build initial status inline in a scope to release the borrow
            let workers = load_balancer.workers.read().await;
            let worker_statuses: Vec<WorkerStatus> =
                workers.iter().map(WorkerStatus::from).collect();
            ProxyStatusResponse {
                version: version.clone(),
                supported_proof_type,
                workers: worker_statuses,
            }
        };

        let (status_tx, status_rx) = watch::channel(initial_status);

        Self {
            load_balancer,
            port,
            status_rx,
            status_tx,
            status_update_interval,
        }
    }
}

#[async_trait]
impl ProxyStatusApi for ProxyStatusPingoraService {
    /// Returns the current status of the proxy.
    #[instrument(target = COMPONENT, name = "proxy.status", skip(_request))]
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
        let updater = ProxyStatusUpdater::new(
            self.load_balancer.clone(),
            self.status_tx.clone(),
            self.status_update_interval,
        );
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
    /// The version of the proxy service.
    version: String,
    /// The supported proof type.
    supported_proof_type: i32,
}

impl ProxyStatusUpdater {
    /// Creates a new [`ProxyStatusUpdater`].
    pub fn new(
        load_balancer: Arc<LoadBalancerState>,
        status_tx: watch::Sender<ProxyStatusResponse>,
        update_interval: Duration,
    ) -> Self {
        let version = env!("CARGO_PKG_VERSION").to_string();
        let supported_proof_type: ProofType = load_balancer.supported_proof_type;
        let supported_proof_type: i32 = supported_proof_type.into();

        Self {
            load_balancer,
            status_tx,
            update_interval,
            version,
            supported_proof_type,
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
                    let new_status = self.build_status().await;
                    let _ = self.status_tx.send(new_status);
                }
                _ = shutdown.changed() => {
                    info!("Status updater received shutdown signal");
                    break;
                }
            }
        }
    }

    /// Build a new status from the load balancer and returns it as a [`ProxyStatusResponse`].
    async fn build_status(&self) -> ProxyStatusResponse {
        let workers = self.load_balancer.workers.read().await;
        let worker_statuses: Vec<WorkerStatus> = workers.iter().map(WorkerStatus::from).collect();

        ProxyStatusResponse {
            version: self.version.clone(),
            supported_proof_type: self.supported_proof_type,
            workers: worker_statuses,
        }
    }
}

// UTILS
// ================================================================================================

impl From<&WorkerHealthStatus> for proto::WorkerHealthStatus {
    fn from(status: &WorkerHealthStatus) -> Self {
        match status {
            WorkerHealthStatus::Healthy => proto::WorkerHealthStatus::Healthy,
            WorkerHealthStatus::Unhealthy { .. } => proto::WorkerHealthStatus::Unhealthy,
            WorkerHealthStatus::Unknown => proto::WorkerHealthStatus::Unknown,
        }
    }
}

impl From<&Worker> for WorkerStatus {
    fn from(worker: &Worker) -> Self {
        Self {
            address: worker.address(),
            version: worker.version().to_string(),
            status: proto::WorkerHealthStatus::from(worker.health_status()).into(),
        }
    }
}
