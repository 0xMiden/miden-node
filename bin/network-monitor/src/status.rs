//! Network monitor status checker.
//!
//! This module contains the logic for checking the status of network services.
//! Individual status checker tasks send updates via watch channels to the web server.

use std::fmt::{self, Display};
use std::time::Duration;

use miden_node_proto::clients::{
    Builder as ClientBuilder,
    RemoteProverProxyStatusClient,
    RpcClient,
};
use miden_node_proto::generated as proto;
use miden_node_proto::generated::rpc::{BlockProducerStatus, RpcStatus, StoreStatus};
use reqwest::Client;
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

/// Details of the increment service.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IncrementDetails {
    /// Number of successful counter increments.
    pub success_count: u64,
    /// Number of failed counter increments.
    pub failure_count: u64,
    /// Last transaction ID (if available).
    pub last_tx_id: Option<String>,
}

/// Details of the counter tracking service.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CounterTrackingDetails {
    /// Current counter value observed on-chain (if available).
    pub current_value: Option<u64>,
    /// Expected counter value based on successful increments sent.
    pub expected_value: Option<u64>,
    /// Last time the counter value was successfully updated.
    pub last_updated: Option<u64>,
    /// Number of pending increments (expected - current).
    pub pending_increments: Option<u64>,
}

/// Details of the explorer service.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExplorerStatusDetails {
    pub block_number: u64,
    pub timestamp: u64,
    pub number_of_transactions: u64,
    pub number_of_nullifiers: u64,
    pub number_of_notes: u64,
    pub number_of_account_updates: u64,
    pub block_commitment: String,
    pub chain_commitment: String,
    pub proof_commitment: String,
}

/// Details of a service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServiceDetails {
    RpcStatus(RpcStatusDetails),
    RemoteProverStatus(RemoteProverStatusDetails),
    RemoteProverTest(ProverTestDetails),
    FaucetTest(FaucetTestDetails),
    NtxIncrement(IncrementDetails),
    NtxTracking(CounterTrackingDetails),
    ExplorerStatus(ExplorerStatusDetails),
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
    /// The block producer's current view of the chain tip height.
    pub chain_tip: u32,
    /// Mempool statistics for this block producer.
    pub mempool: MempoolStatusDetails,
}

/// Details about the block producer's mempool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MempoolStatusDetails {
    /// Number of transactions currently in the mempool waiting to be batched.
    pub unbatched_transactions: u64,
    /// Number of batches currently being proven.
    pub proposed_batches: u64,
    /// Number of proven batches waiting for block inclusion.
    pub proven_batches: u64,
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
    pub name: String,
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
        // We assume all supported nodes expose mempool statistics.
        let mempool_stats = value
            .mempool_stats
            .expect("block producer status must include mempool statistics");

        Self {
            version: value.version,
            status: value.status.into(),
            chain_tip: value.chain_tip,
            mempool: MempoolStatusDetails {
                unbatched_transactions: mempool_stats.unbatched_transactions,
                proposed_batches: mempool_stats.proposed_batches,
                proven_batches: mempool_stats.proven_batches,
            },
        }
    }
}

impl From<proto::remote_prover::ProxyWorkerStatus> for WorkerStatusDetails {
    fn from(value: proto::remote_prover::ProxyWorkerStatus) -> Self {
        let status =
            proto::remote_prover::WorkerHealthStatus::try_from(value.status).unwrap().into();

        Self {
            name: value.name,
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
    request_timeout: Duration,
) {
    let mut rpc = ClientBuilder::new(rpc_url)
        .with_tls()
        .expect("TLS is enabled")
        .with_timeout(request_timeout)
        .without_metadata_version()
        .without_metadata_genesis()
        .without_otel_context_injection()
        .connect_lazy::<RpcClient>();

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
    request_timeout: Duration,
) {
    let url_str = prover_url.to_string();
    let mut remote_prover = ClientBuilder::new(prover_url)
        .with_tls()
        .expect("TLS is enabled")
        .with_timeout(request_timeout)
        .without_metadata_version()
        .without_metadata_genesis()
        .without_otel_context_injection()
        .connect_lazy::<RemoteProverProxyStatusClient>();

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
    display_name: String,
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
                name: display_name.clone(),
                status: overall_health,
                last_checked: current_time,
                error: None,
                details: ServiceDetails::RemoteProverStatus(remote_prover_details),
            }
        },
        Err(e) => ServiceStatus {
            name: display_name,
            status: Status::Unhealthy,
            last_checked: current_time,
            error: Some(e.to_string()),
            details: ServiceDetails::Error,
        },
    }
}

// EXPLORER STATUS CHECKER
// ================================================================================================

const LATEST_BLOCK_QUERY: &str = "
query LatestBlock {
    blocks(input: { sort_by: timestamp, order_by: desc }, first: 1) {
        edges {
            node {
                block_number
                timestamp
                number_of_transactions
                number_of_nullifiers
                number_of_notes
                block_commitment
                chain_commitment
                proof_commitment
                number_of_account_updates
            }
        }
    }
}
";

#[derive(Serialize, Copy, Clone)]
struct EmptyVariables;

#[derive(Serialize, Copy, Clone)]
struct GraphqlRequest<V> {
    query: &'static str,
    variables: V,
}

const LATEST_BLOCK_REQUEST: GraphqlRequest<EmptyVariables> = GraphqlRequest {
    query: LATEST_BLOCK_QUERY,
    variables: EmptyVariables,
};

/// Runs a task that continuously checks explorer status and updates a watch channel.
///
/// This function spawns a task that periodically checks the explorer service status
/// and sends updates through a watch channel.
///
/// # Arguments
///
/// * `explorer_url` - The URL of the explorer service.
/// * `name` - The name of the explorer.
/// * `status_sender` - The sender for the watch channel.
/// * `status_check_interval` - The interval at which to check the status of the services.
///
/// # Returns
///
/// `Ok(())` if the monitoring task runs and completes successfully, or an error if there are
/// connection issues or failures while checking the explorer status.
#[instrument(target = COMPONENT, name = "explorer-status-task", skip_all)]
pub async fn run_explorer_status_task(
    explorer_url: Url,
    name: String,
    status_sender: watch::Sender<ServiceStatus>,
    status_check_interval: Duration,
    request_timeout: Duration,
) {
    let mut explorer_client = reqwest::Client::new();

    let mut interval = tokio::time::interval(status_check_interval);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        let current_time = current_unix_timestamp_secs();

        let status = check_explorer_status(
            &mut explorer_client,
            explorer_url.clone(),
            name.clone(),
            current_time,
            request_timeout,
        )
        .await;

        // Send the status update; exit if no receivers (shutdown signal)
        if status_sender.send(status).is_err() {
            info!("No receivers for explorer status updates, shutting down");
            return;
        }
    }
}

/// Checks the status of the explorer service.
///
/// This function checks the status of the explorer service.
///
/// # GraphQL Query
///
/// ```graphql
/// query LatestBlock {
///     blocks(input: { sort_by: timestamp, order_by: desc }, first: 1) {
///         edges {
///             node {
///                 id
///                 block_number
///                 timestamp
///                 number_of_transactions
///                 number_of_nullifiers
///                 number_of_notes
///             }
///         }
///     }
/// }
/// ```
///
/// # Arguments
///
/// * `explorer` - The explorer client.
/// * `name` - The name of the explorer.
/// * `url` - The URL of the explorer.
/// * `current_time` - The current time.
///
/// # Returns
///
/// A `ServiceStatus` containing the status of the explorer service.
#[instrument(target = COMPONENT, name = "check-status.explorer", skip_all, ret(level = "info"))]
pub(crate) async fn check_explorer_status(
    explorer_client: &mut Client,
    explorer_url: Url,
    name: String,
    current_time: u64,
    request_timeout: Duration,
) -> ServiceStatus {
    match explorer_client
        .post(explorer_url.to_string())
        .json(&LATEST_BLOCK_REQUEST)
        .timeout(request_timeout)
        .send()
        .await
    {
        Ok(response) => match response.json::<serde_json::Value>().await {
            Ok(status) => match ExplorerStatusDetails::try_from(status) {
                Ok(explorer_details) => ServiceStatus {
                    name: name.clone(),
                    status: Status::Healthy,
                    last_checked: current_time,
                    error: None,
                    details: ServiceDetails::ExplorerStatus(explorer_details),
                },
                Err(err) => ServiceStatus {
                    name: name.clone(),
                    status: Status::Unhealthy,
                    last_checked: current_time,
                    error: Some(err.to_string()),
                    details: ServiceDetails::Error,
                },
            },
            Err(err) => ServiceStatus {
                name: name.clone(),
                status: Status::Unhealthy,
                last_checked: current_time,
                error: Some(err.to_string()),
                details: ServiceDetails::Error,
            },
        },
        Err(e) => ServiceStatus {
            name: name.clone(),
            status: Status::Unhealthy,
            last_checked: current_time,
            error: Some(e.to_string()),
            details: ServiceDetails::Error,
        },
    }
}

#[derive(Debug)]
pub enum ExplorerStatusError {
    MissingField(String),
}

impl Display for ExplorerStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExplorerStatusError::MissingField(field) => write!(f, "missing field: {field}"),
        }
    }
}

impl TryFrom<serde_json::Value> for ExplorerStatusDetails {
    type Error = ExplorerStatusError;

    fn try_from(value: serde_json::Value) -> Result<Self, Self::Error> {
        let node = value.pointer("/data/blocks/edges/0/node").ok_or_else(|| {
            ExplorerStatusError::MissingField("data.blocks.edges[0].node".to_string())
        })?;

        let block_number = node
            .get("block_number")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| ExplorerStatusError::MissingField("block_number".to_string()))?;
        let timestamp = node
            .get("timestamp")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| ExplorerStatusError::MissingField("timestamp".to_string()))?;

        let number_of_transactions = node
            .get("number_of_transactions")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                ExplorerStatusError::MissingField("number_of_transactions".to_string())
            })?;
        let number_of_nullifiers = node
            .get("number_of_nullifiers")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| ExplorerStatusError::MissingField("number_of_nullifiers".to_string()))?;
        let number_of_notes = node
            .get("number_of_notes")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| ExplorerStatusError::MissingField("number_of_notes".to_string()))?;
        let number_of_account_updates = node
            .get("number_of_account_updates")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                ExplorerStatusError::MissingField("number_of_account_updates".to_string())
            })?;

        let block_commitment = node
            .get("block_commitment")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ExplorerStatusError::MissingField("block_commitment".to_string()))?
            .to_string();
        let chain_commitment = node
            .get("chain_commitment")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ExplorerStatusError::MissingField("chain_commitment".to_string()))?
            .to_string();
        let proof_commitment = node
            .get("proof_commitment")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ExplorerStatusError::MissingField("proof_commitment".to_string()))?
            .to_string();

        Ok(Self {
            block_number,
            timestamp,
            number_of_transactions,
            number_of_nullifiers,
            number_of_notes,
            number_of_account_updates,
            block_commitment,
            chain_commitment,
            proof_commitment,
        })
    }
}

pub(crate) fn initial_explorer_status() -> ServiceStatus {
    ServiceStatus {
        name: "Explorer".to_string(),
        status: Status::Unknown,
        last_checked: current_unix_timestamp_secs(),
        error: None,
        details: ServiceDetails::ExplorerStatus(ExplorerStatusDetails::default()),
    }
}
