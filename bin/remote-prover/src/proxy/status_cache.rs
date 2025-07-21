//! Status caching logic for the proxy status endpoint.
//!
//! This module provides a `ProxyStatusCache` struct that manages a cached
//! `ProxyStatusResponse` for efficient status serving in the proxy pipeline.

use std::sync::Arc;

use miden_remote_prover::generated::remote_prover::{ProxyStatusResponse, WorkerStatus};
use tokio::sync::RwLock;

use crate::proxy::worker::{Worker, WorkerHealthStatus};

/// Caches the proxy status response.
#[derive(Debug)]
pub struct ProxyStatusCache {
    cached_status: Arc<RwLock<ProxyStatusResponse>>,
}

impl ProxyStatusCache {
    /// Create a new status cache with the given initial status.
    pub fn new(initial_status: ProxyStatusResponse) -> Self {
        Self {
            cached_status: Arc::new(RwLock::new(initial_status)),
        }
    }

    /// Get the cached status response.
    pub async fn get_cached_status(&self) -> ProxyStatusResponse {
        self.cached_status.read().await.clone()
    }

    /// Update the cached status response.
    pub async fn update_status(&self, new_status: ProxyStatusResponse) {
        *self.cached_status.write().await = new_status;
    }
}

/// Conversion from a Worker reference to a `WorkerStatus` proto message.
impl From<&Worker> for WorkerStatus {
    fn from(worker: &Worker) -> Self {
        use miden_remote_prover::generated::remote_prover::WorkerHealthStatus as ProtoWorkerHealthStatus;
        Self {
            address: worker.address(),
            version: worker.version().to_string(),
            status: match worker.health_status() {
                WorkerHealthStatus::Healthy => ProtoWorkerHealthStatus::Healthy,
                WorkerHealthStatus::Unhealthy { .. } => ProtoWorkerHealthStatus::Unhealthy,
                WorkerHealthStatus::Unknown => ProtoWorkerHealthStatus::Unknown,
            } as i32,
        }
    }
}
