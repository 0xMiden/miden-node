//! Network monitor status checker.
//!
//! This module contains the logic for checking the status of the network monitor.
//! It is used to check the status of the network monitor and to update the shared status.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use miden_node_proto::clients::{Builder as ClientBuilder, RemoteProverProxy, Rpc};
use miden_node_proto::generated::block_producer::BlockProducerStatus;
use miden_node_proto::generated::remote_prover::{
    ProofType,
    ProxyStatus,
    ProxyWorkerStatus,
    WorkerHealthStatus,
};
use miden_node_proto::generated::rpc::RpcStatus;
use miden_node_proto::generated::rpc_store::StoreStatus;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, watch};
use tokio::time::MissedTickBehavior;
use tracing::instrument;
use url::Url;

use crate::COMPONENT;

// MONITOR CONFIGURATION
// ================================================================================================

const DEFAULT_RPC_URL: &str = "http://localhost:50051";
const DEFAULT_REMOTE_PROVER_URLS: &str = "http://localhost:50052";
const DEFAULT_PORT: u16 = 3000;

const RPC_URL_ENV_VAR: &str = "MIDEN_MONITOR_RPC_URL";
const REMOTE_PROVER_URLS_ENV_VAR: &str = "MIDEN_MONITOR_REMOTE_PROVER_URLS";
const PORT_ENV_VAR: &str = "MIDEN_MONITOR_PORT";

/// Configuration for the monitor.
///
/// This struct contains the configuration for the monitor.
#[derive(Debug, Clone)]
pub struct MonitorConfig {
    /// The URL of the RPC service.
    pub rpc_url: Url,
    /// The URLs of the remote provers.
    pub remote_prover_urls: Vec<Url>,
    /// The port of the monitor.
    pub port: u16,
}

impl MonitorConfig {
    /// Loads the configuration from the environment variables.
    ///
    /// This function loads the configuration from the environment variables.
    /// The environment variables are:
    /// - `MIDEN_MONITOR_RPC_URL`: The URL of the RPC service.
    /// - `MIDEN_MONITOR_REMOTE_PROVER_URLS`: The URLs of the remote provers, comma separated.
    /// - `MIDEN_MONITOR_PORT`: The port of the monitor.
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let rpc_url =
            std::env::var(RPC_URL_ENV_VAR).unwrap_or_else(|_| DEFAULT_RPC_URL.to_string());

        // Parse multiple remote prover URLs from environment variable
        let remote_prover_urls = std::env::var(REMOTE_PROVER_URLS_ENV_VAR)
            .unwrap_or_else(|_| DEFAULT_REMOTE_PROVER_URLS.to_string());

        let remote_prover_urls = remote_prover_urls
            .split(',')
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(Url::parse)
            .collect::<Result<Vec<_>, _>>()?;

        let port = std::env::var(PORT_ENV_VAR)
            .unwrap_or_else(|_| DEFAULT_PORT.to_string())
            .parse::<u16>()?;

        Ok(MonitorConfig {
            rpc_url: Url::parse(&rpc_url)?,
            remote_prover_urls,
            port,
        })
    }
}

// SERVICE STATUS CHECKER
// ================================================================================================

/// Status of a service.
///
/// This struct contains the status of a service, the last time it was checked, and any errors that
/// occurred. It also contains the details of the service, which is a union of the details of the
/// service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub name: String,
    pub status: String,
    pub last_checked: u64,
    pub error: Option<String>,
    pub details: Option<ServiceDetails>,
}

/// Details of a service.
///
/// This struct contains the details of a service, which is a union of the details of the service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDetails {
    pub rpc_status: Option<RpcStatusDetails>,
    pub remote_prover_statuses: Vec<RemoteProverStatusDetails>,
}

/// Details of an RPC service.
///
/// This struct contains the details of an RPC service, which is a union of the details of the RPC
/// service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcStatusDetails {
    pub version: String,
    pub genesis_commitment: Option<String>,
    pub store_status: Option<StoreStatusDetails>,
    pub block_producer_status: Option<BlockProducerStatusDetails>,
}

/// Details of a store service.
///
/// This struct contains the details of a store service, which is a union of the details of the
/// store service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreStatusDetails {
    pub version: String,
    pub status: String,
    pub chain_tip: u32,
}

/// Details of a block producer service.
///
/// This struct contains the details of a block producer service, which is a union of the details
/// of the block producer service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockProducerStatusDetails {
    pub version: String,
    pub status: String,
}

/// Details of a remote prover service.
///
/// This struct contains the details of a remote prover service, which is a union of the details
/// of the remote prover service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteProverStatusDetails {
    pub name: String,
    pub url: String,
    pub version: String,
    pub supported_proof_type: String,
    pub workers: Vec<WorkerStatusDetails>,
}

/// Details of a worker service.
///
/// This struct contains the details of a worker service, which is a union of the details of the
/// worker service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerStatusDetails {
    pub address: String,
    pub version: String,
    pub status: String,
}

/// Status of a network.
///
/// This struct contains the status of a network, which is a union of the status of the network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkStatus {
    pub services: Vec<ServiceStatus>,
    pub last_updated: u64,
}

/// Shared status of the network.
///
/// This struct contains the shared status of the network, which is a union of the shared status of
/// the network.
pub type SharedStatus = Arc<Mutex<NetworkStatus>>;

// FROM IMPLEMENTATIONS
// ================================================================================================

/// From implementations for converting gRPC types to domain types
///
/// This implementation converts a `StoreStatus` to a `StoreStatusDetails`.
impl From<StoreStatus> for StoreStatusDetails {
    fn from(status: StoreStatus) -> Self {
        Self {
            version: status.version,
            status: status.status,
            chain_tip: status.chain_tip,
        }
    }
}

impl From<BlockProducerStatus> for BlockProducerStatusDetails {
    fn from(status: BlockProducerStatus) -> Self {
        Self {
            version: status.version,
            status: status.status,
        }
    }
}

impl From<ProxyWorkerStatus> for WorkerStatusDetails {
    fn from(worker: ProxyWorkerStatus) -> Self {
        let status_str = match WorkerHealthStatus::try_from(worker.status) {
            Ok(WorkerHealthStatus::Healthy) => "HEALTHY",
            Ok(WorkerHealthStatus::Unhealthy) => "UNHEALTHY",
            Ok(WorkerHealthStatus::Unknown) | Err(_) => "UNKNOWN",
        };

        Self {
            address: worker.address,
            version: worker.version,
            status: status_str.to_string(),
        }
    }
}

impl RemoteProverStatusDetails {
    pub fn from_proxy_status(status: ProxyStatus, name: String, url: String) -> Self {
        let proof_type_str = match ProofType::try_from(status.supported_proof_type) {
            Ok(ProofType::Transaction) => "TRANSACTION",
            Ok(ProofType::Batch) => "BATCH",
            Ok(ProofType::Block) => "BLOCK",
            Err(_) => "UNKNOWN",
        };

        let workers: Vec<WorkerStatusDetails> =
            status.workers.into_iter().map(WorkerStatusDetails::from).collect();

        Self {
            name,
            url,
            version: status.version,
            supported_proof_type: proof_type_str.to_string(),
            workers,
        }
    }
}

impl From<RpcStatus> for RpcStatusDetails {
    fn from(status: RpcStatus) -> Self {
        Self {
            version: status.version,
            genesis_commitment: status.genesis_commitment.as_ref().map(|gc| format!("{gc:?}")),
            store_status: status.store.map(StoreStatusDetails::from),
            block_producer_status: status.block_producer.map(BlockProducerStatusDetails::from),
        }
    }
}

// RPC STATUS CHECKER
// ================================================================================================

/// Runs a task that continuously checks RPC status and updates a watch channel.
///
/// This function spawns a task that periodically checks the RPC service status
/// and sends updates through a watch channel.
///
/// # Arguments
///
/// * `rpc_url` - The URL of the RPC service.
/// * `status_sender` - The sender for the watch channel.
///
/// # Returns
///
/// `Ok(())` if the task completes successfully, or an error if the task fails.
#[instrument(target = COMPONENT, name = "rpc-status-task", skip_all)]
async fn run_rpc_status_task(
    rpc_url: Url,
    status_sender: watch::Sender<ServiceStatus>,
) -> anyhow::Result<()> {
    let mut rpc = ClientBuilder::new(rpc_url)
        .without_tls()
        .without_timeout()
        .without_metadata_version()
        .without_metadata_genesis()
        .connect_lazy::<Rpc>();

    let mut interval = tokio::time::interval(Duration::from_secs(3));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("failed to get current time")?
            .as_secs();

        let status = check_rpc_status(&mut rpc, current_time).await;

        // Send the status update (ignore if no receivers)
        let _ = status_sender.send(status);
    }
}

/// Checks the status of the RPC service.
///
/// This function checks the status of the RPC service.
///
/// # Arguments
///
/// * `rpc` - The RPC client.
/// * `current_time` - The current time.
///
/// # Returns
///
/// A `ServiceStatus` containing the status of the RPC service.
#[instrument(target = COMPONENT, name = "check-status.rpc", skip_all, ret(level = "info"))]
async fn check_rpc_status(
    rpc: &mut miden_node_proto::clients::RpcClient,
    current_time: u64,
) -> ServiceStatus {
    match rpc.status(()).await {
        Ok(response) => {
            let status = response.into_inner();

            ServiceStatus {
                name: "RPC".to_string(),
                status: "healthy".to_string(),
                last_checked: current_time,
                error: None,
                details: Some(ServiceDetails {
                    rpc_status: Some(status.into()),
                    remote_prover_statuses: Vec::new(),
                }),
            }
        },
        Err(e) => ServiceStatus {
            name: "RPC".to_string(),
            status: "unhealthy".to_string(),
            last_checked: current_time,
            error: Some(e.to_string()),
            details: None,
        },
    }
}

// REMOTE PROVER STATUS CHECKER
// ================================================================================================

/// Runs a task that continuously checks remote prover status and updates a watch channel.
///
/// This function spawns a task that periodically checks a remote prover service status
/// and sends updates through a watch channel.
///
/// # Arguments
///
/// * `prover_url` - The URL of the remote prover service.
/// * `name` - The name of the remote prover.
/// * `status_sender` - The sender for the watch channel.
///
/// # Returns
///
/// `Ok(())` if the monitoring task runs and completes successfully, or an error if there are
/// connection issues or failures while checking the remote prover status.
#[instrument(target = COMPONENT, name = "remote-prover-status-task", skip_all)]
async fn run_remote_prover_status_task(
    prover_url: Url,
    name: String,
    status_sender: watch::Sender<ServiceStatus>,
) -> anyhow::Result<()> {
    let url_str = prover_url.to_string();
    let mut remote_prover = ClientBuilder::new(prover_url)
        .without_tls()
        .without_timeout()
        .without_metadata_version()
        .without_metadata_genesis()
        .connect_lazy::<RemoteProverProxy>();

    let mut interval = tokio::time::interval(Duration::from_secs(3));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("failed to get current time")?
            .as_secs();

        let status = check_remote_prover_status(
            &mut remote_prover,
            name.clone(),
            url_str.clone(),
            current_time,
        )
        .await;

        // Send the status update (ignore if no receivers)
        let _ = status_sender.send(status);
    }
}

/// Checks the status of the remote prover service.
///
/// This function checks the status of the remote prover service.
///
/// # Arguments
///
/// * `remote_prover` - The remote prover client.
/// * `name` - The name of the remote prover.
/// * `url` - The URL of the remote prover.
/// * `current_time` - The current time.
///
/// # Returns
///
/// A `ServiceStatus` containing the status of the remote prover service.
#[instrument(target = COMPONENT, name = "check-status.remote-prover", skip_all, ret(level = "info"))]
async fn check_remote_prover_status(
    remote_prover: &mut miden_node_proto::clients::RemoteProverProxyClient,
    name: String,
    url: String,
    current_time: u64,
) -> ServiceStatus {
    match remote_prover.status(()).await {
        Ok(response) => {
            let status = response.into_inner();

            // Use the new method to convert gRPC status to domain type
            let remote_prover_details =
                RemoteProverStatusDetails::from_proxy_status(status, name.clone(), url);

            // Determine overall health based on worker statuses
            let overall_health = if remote_prover_details.workers.is_empty() {
                "unknown"
            } else if remote_prover_details.workers.iter().any(|w| w.status == "HEALTHY") {
                "healthy"
            } else {
                "unhealthy"
            };

            ServiceStatus {
                name: format!("Remote Prover ({name})"),
                status: overall_health.to_string(),
                last_checked: current_time,
                error: None,
                details: Some(ServiceDetails {
                    rpc_status: None,
                    remote_prover_statuses: vec![remote_prover_details],
                }),
            }
        },
        Err(e) => ServiceStatus {
            name: format!("Remote Prover ({name})"),
            status: "unhealthy".to_string(),
            last_checked: current_time,
            error: Some(e.to_string()),
            details: None,
        },
    }
}

// NETWORK STATUS CHECKER
// ================================================================================================

/// Updates the shared status with the latest statuses from all services.
///
/// This helper function collects the current status from all watch channels
/// and updates the shared network status atomically.
async fn update_shared_status(
    shared_status: &SharedStatus,
    rpc_status_rx: &watch::Receiver<ServiceStatus>,
    prover_status_receivers: &[watch::Receiver<ServiceStatus>],
) {
    let current_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs();

    let mut services = Vec::new();

    // Collect RPC status
    services.push(rpc_status_rx.borrow().clone());

    // Collect all remote prover statuses
    for rx in prover_status_receivers {
        services.push(rx.borrow().clone());
    }

    // Update shared status atomically
    {
        let mut status = shared_status.lock().await;
        status.services = services;
        status.last_updated = current_time;
    }
}

/// Coordinates status checking across multiple services using separate tasks.
///
/// This function spawns individual tasks for each service (RPC and remote provers)
/// that continuously check their respective statuses and communicate via watch channels.
/// This coordinator receives updates from all services and maintains the shared status.
///
/// # Arguments
///
/// * `shared_status` - The shared status of the network.
/// * `config` - The configuration for the monitor.
///
/// # Returns
///
/// `Ok(())` when the status monitoring coordinator shuts down gracefully. Returns an error if
/// there are critical failures such as:
/// - Failed to acquire locks on the shared status during updates
/// - Watch channel communication between monitoring tasks and coordinator fails
/// - The main coordinator loop encounters an unrecoverable error while processing status updates
#[instrument(target = COMPONENT, name = "check-status", skip_all, ret(level = "info"), err)]
pub async fn check_status(
    shared_status: SharedStatus,
    config: MonitorConfig,
) -> anyhow::Result<()> {
    // Create initial empty status for each service
    let initial_rpc_status = ServiceStatus {
        name: "RPC".to_string(),
        status: "unknown".to_string(),
        last_checked: 0,
        error: None,
        details: None,
    };

    // Create watch channels and spawn RPC status task
    let (rpc_status_tx, mut rpc_status_rx) = watch::channel(initial_rpc_status);
    let rpc_url = config.rpc_url.clone();
    let mut rpc_task_handle = Some(tokio::spawn(async move {
        if let Err(e) = run_rpc_status_task(rpc_url, rpc_status_tx).await {
            tracing::error!(target: COMPONENT, "RPC status task failed: {}", e);
        }
    }));

    // Create watch channels and spawn remote prover status tasks
    let mut prover_status_receivers: Vec<watch::Receiver<ServiceStatus>> = Vec::new();
    let mut prover_task_handles = Vec::new();

    for (i, prover_url) in config.remote_prover_urls.iter().enumerate() {
        let name = format!("Prover-{}", i + 1);
        let initial_prover_status = ServiceStatus {
            name: format!("Remote Prover ({name})"),
            status: "unknown".to_string(),
            last_checked: 0,
            error: None,
            details: None,
        };

        let (prover_status_tx, prover_status_rx) = watch::channel(initial_prover_status);
        prover_status_receivers.push(prover_status_rx);

        let prover_url_clone = prover_url.clone();
        let name_clone = name.clone();
        let prover_task_handle = tokio::spawn(async move {
            if let Err(e) =
                run_remote_prover_status_task(prover_url_clone, name_clone, prover_status_tx).await
            {
                tracing::error!(target: COMPONENT, "Remote prover status task failed: {}", e);
            }
        });
        prover_task_handles.push(prover_task_handle);
    }

    // Main coordinator loop - react to status changes immediately
    loop {
        tokio::select! {
            // RPC status changed - update immediately
            _ = rpc_status_rx.changed() => {
                update_shared_status(&shared_status, &rpc_status_rx, &prover_status_receivers).await;
            },
            // Any prover status changed - update immediately
            _ = async {
                for rx in &mut prover_status_receivers {
                    if rx.changed().await.is_ok() {
                        return;
                    }
                }
            } => {
                update_shared_status(&shared_status, &rpc_status_rx, &prover_status_receivers).await;
            },
            // Handle RPC task failure
            result = async {
                match rpc_task_handle.as_mut() {
                    Some(handle) => handle.await,
                    None => std::future::pending().await,
                }
            } => {
                match result {
                    Ok(_) => anyhow::bail!("RPC status task unexpectedly terminated"),
                    Err(e) => anyhow::bail!("RPC status task panicked: {}", e),
                }
            },
        }
    }
}
