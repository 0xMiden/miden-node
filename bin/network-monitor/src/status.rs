//! Network monitor status checker.
//!
//! This module contains the logic for checking the status of network services.
//! Individual status checker tasks send updates via watch channels to the web server.

use std::time::Duration;

use miden_node_proto::clients::{Builder as ClientBuilder, RemoteProverProxy, Rpc};
use miden_node_proto::generated as proto;
use miden_node_proto::generated::block_producer::BlockProducerStatus;
use miden_node_proto::generated::rpc::RpcStatus;
use miden_node_proto::generated::rpc_store::StoreStatus;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;
use tracing::{info, instrument};
use url::Url;

use crate::faucet::FaucetTestDetails;
use crate::remote_prover::{ProofType, ProverTestDetails};
use crate::{COMPONENT, current_unix_timestamp_secs};

// STATUS
// ================================================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Status {
    Healthy,
    Unhealthy,
    Unknown,
}

impl From<String> for Status {
    fn from(value: String) -> Self {
        match value.as_str() {
            "HEALTHY" | "connected" => Status::Healthy,
            "UNHEALTHY" | "disconnected" => Status::Unhealthy,
            _ => Status::Unknown,
        }
    }
}

impl From<proto::remote_prover::WorkerHealthStatus> for Status {
    fn from(value: proto::remote_prover::WorkerHealthStatus) -> Self {
        match value {
            proto::remote_prover::WorkerHealthStatus::Unknown => Status::Unknown,
            proto::remote_prover::WorkerHealthStatus::Healthy => Status::Healthy,
            proto::remote_prover::WorkerHealthStatus::Unhealthy => Status::Unhealthy,
        }
    }
}

// SERVICE STATUS
// ================================================================================================

/// Status of a service.
///
/// This struct contains the status of a service, the last time it was checked, and any errors that
/// occurred. It also contains the details of the service, which is a union of the details of the
/// service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub name: String,
    pub status: Status,
    pub last_checked: u64,
    pub error: Option<String>,
    pub details: ServiceDetails,
}

/// Details of a service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServiceDetails {
    RpcStatus(RpcStatusDetails),
    RemoteProverStatus(RemoteProverStatusDetails),
    RemoteProverTest(ProverTestDetails),
    FaucetTest(FaucetTestDetails),
    Error,
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
    pub status: Status,
    pub chain_tip: u32,
}

/// Details of a block producer service.
///
/// This struct contains the details of a block producer service, which is a union of the details
/// of the block producer service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockProducerStatusDetails {
    pub version: String,
    pub status: Status,
}

/// Details of a remote prover service.
///
/// This struct contains the details of a remote prover service, which is a union of the details
/// of the remote prover service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteProverStatusDetails {
    pub url: String,
    pub version: String,
    pub supported_proof_type: ProofType,
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
    pub status: Status,
}

/// Status of a network.
///
/// This struct contains the status of a network, which is a union of the status of the network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkStatus {
    pub services: Vec<ServiceStatus>,
    pub last_updated: u64,
}

// FROM IMPLEMENTATIONS
// ================================================================================================

/// From implementations for converting gRPC types to domain types
///
/// This implementation converts a `StoreStatus` to a `StoreStatusDetails`.
impl From<StoreStatus> for StoreStatusDetails {
    fn from(value: StoreStatus) -> Self {
        Self {
            version: value.version,
            status: value.status.into(),
            chain_tip: value.chain_tip,
        }
    }
}

impl From<BlockProducerStatus> for BlockProducerStatusDetails {
    fn from(value: BlockProducerStatus) -> Self {
        Self {
            version: value.version,
            status: value.status.into(),
        }
    }
}

impl From<proto::remote_prover::ProxyWorkerStatus> for WorkerStatusDetails {
    fn from(value: proto::remote_prover::ProxyWorkerStatus) -> Self {
        let status =
            proto::remote_prover::WorkerHealthStatus::try_from(value.status).unwrap().into();

        Self {
            address: value.address,
            version: value.version,
            status,
        }
    }
}

impl RemoteProverStatusDetails {
    pub fn from_proxy_status(status: proto::remote_prover::ProxyStatus, url: String) -> Self {
        let proof_type = proto::remote_prover::ProofType::try_from(status.supported_proof_type)
            .unwrap()
            .into();

        let workers: Vec<WorkerStatusDetails> =
            status.workers.into_iter().map(WorkerStatusDetails::from).collect();

        Self {
            url,
            version: status.version,
            supported_proof_type: proof_type,
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
/// * `status_check_interval` - The interval at which to check the status of the services.
///
/// # Returns
///
/// `Ok(())` if the task completes successfully, or an error if the task fails.
#[instrument(target = COMPONENT, name = "rpc-status-task", skip_all)]
pub async fn run_rpc_status_task(
    rpc_url: Url,
    status_sender: watch::Sender<ServiceStatus>,
    status_check_interval: Duration,
) {
    let mut rpc = ClientBuilder::new(rpc_url)
        .with_tls()
        .expect("TLS is enabled")
        .with_timeout(Duration::from_secs(10))
        .without_metadata_version()
        .without_metadata_genesis()
        .connect_lazy::<Rpc>();

    let mut interval = tokio::time::interval(status_check_interval);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        let current_time = current_unix_timestamp_secs();

        let status = check_rpc_status(&mut rpc, current_time).await;

        // Send the status update; exit if no receivers (shutdown signal)
        if status_sender.send(status).is_err() {
            info!("No receivers for RPC status updates, shutting down");
            return;
        }
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
pub(crate) async fn check_rpc_status(
    rpc: &mut miden_node_proto::clients::RpcClient,
    current_time: u64,
) -> ServiceStatus {
    match rpc.status(()).await {
        Ok(response) => {
            let status = response.into_inner();

            ServiceStatus {
                name: "RPC".to_string(),
                status: Status::Healthy,
                last_checked: current_time,
                error: None,
                details: ServiceDetails::RpcStatus(status.into()),
            }
        },
        Err(e) => ServiceStatus {
            name: "RPC".to_string(),
            status: Status::Unhealthy,
            last_checked: current_time,
            error: Some(e.to_string()),
            details: ServiceDetails::Error,
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
/// * `status_check_interval` - The interval at which to check the status of the services.
///
/// # Returns
///
/// `Ok(())` if the monitoring task runs and completes successfully, or an error if there are
/// connection issues or failures while checking the remote prover status.
#[instrument(target = COMPONENT, name = "remote-prover-status-task", skip_all)]
pub async fn run_remote_prover_status_task(
    prover_url: Url,
    name: String,
    status_sender: watch::Sender<ServiceStatus>,
    status_check_interval: Duration,
) {
    let url_str = prover_url.to_string();
    let mut remote_prover = ClientBuilder::new(prover_url)
        .with_tls()
        .expect("TLS is enabled")
        .with_timeout(Duration::from_secs(10))
        .without_metadata_version()
        .without_metadata_genesis()
        .connect_lazy::<RemoteProverProxy>();

    let mut interval = tokio::time::interval(status_check_interval);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        let current_time = current_unix_timestamp_secs();

        let status = check_remote_prover_status(
            &mut remote_prover,
            name.clone(),
            url_str.clone(),
            current_time,
        )
        .await;

        // Send the status update; exit if no receivers (shutdown signal)
        if status_sender.send(status).is_err() {
            info!("No receivers for remote prover status updates, shutting down");
            return;
        }
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
pub(crate) async fn check_remote_prover_status(
    remote_prover: &mut miden_node_proto::clients::RemoteProverProxyStatusClient,
    name: String,
    url: String,
    current_time: u64,
) -> ServiceStatus {
    match remote_prover.status(()).await {
        Ok(response) => {
            let status = response.into_inner();

            // Use the new method to convert gRPC status to domain type
            let remote_prover_details = RemoteProverStatusDetails::from_proxy_status(status, url);

            // Determine overall health based on worker statuses
            let overall_health = if remote_prover_details.workers.is_empty() {
                Status::Unknown
            } else if remote_prover_details.workers.iter().any(|w| w.status == Status::Healthy) {
                Status::Healthy
            } else {
                Status::Unhealthy
            };

            ServiceStatus {
                name: format!("Remote Prover ({name})"),
                status: overall_health,
                last_checked: current_time,
                error: None,
                details: ServiceDetails::RemoteProverStatus(remote_prover_details),
            }
        },
        Err(e) => ServiceStatus {
            name: format!("Remote Prover ({name})"),
            status: Status::Unhealthy,
            last_checked: current_time,
            error: Some(e.to_string()),
            details: ServiceDetails::Error,
        },
    }
}
