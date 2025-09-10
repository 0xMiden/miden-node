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
use tokio::sync::Mutex;
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub name: String,
    pub status: String,
    pub last_checked: u64,
    pub error: Option<String>,
    pub details: Option<ServiceDetails>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDetails {
    pub rpc_status: Option<RpcStatusDetails>,
    pub remote_prover_statuses: Vec<RemoteProverStatusDetails>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcStatusDetails {
    pub version: String,
    pub genesis_commitment: Option<String>,
    pub store_status: Option<StoreStatusDetails>,
    pub block_producer_status: Option<BlockProducerStatusDetails>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreStatusDetails {
    pub version: String,
    pub status: String,
    pub chain_tip: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockProducerStatusDetails {
    pub version: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteProverStatusDetails {
    pub name: String,
    pub url: String,
    pub version: String,
    pub supported_proof_type: String,
    pub workers: Vec<WorkerStatusDetails>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerStatusDetails {
    pub address: String,
    pub version: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkStatus {
    pub services: Vec<ServiceStatus>,
    pub last_updated: u64,
}

pub type SharedStatus = Arc<Mutex<NetworkStatus>>;

// From implementations for converting gRPC types to domain types
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

#[derive(Debug, Clone)]
pub struct MonitoringConfig {
    pub rpc_url: Url,
    pub remote_prover_urls: Vec<Url>,
    pub port: u16,
}

impl MonitoringConfig {
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let rpc_url = std::env::var("MIDEN_MONITORING_RPC_URL")
            .unwrap_or_else(|_| "http://localhost:50051".to_string());

        // Parse multiple remote prover URLs from environment variable
        let remote_prover_urls = std::env::var("MIDEN_MONITORING_REMOTE_PROVER_URLS")
            .unwrap_or_else(|_| "http://localhost:50052".to_string());

        let remote_prover_urls = remote_prover_urls
            .split(',')
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(Url::parse)
            .collect::<Result<Vec<_>, _>>()?;

        let port = std::env::var("MIDEN_MONITORING_PORT")
            .unwrap_or_else(|_| "3000".to_string())
            .parse::<u16>()?;

        Ok(MonitoringConfig {
            rpc_url: Url::parse(&rpc_url)?,
            remote_prover_urls,
            port,
        })
    }
}

pub async fn check_rpc_status(
    rpc: &mut miden_node_proto::clients::RpcClient,
    current_time: u64,
) -> ServiceStatus {
    match rpc.status(()).await {
        Ok(response) => {
            let status = response.into_inner();

            // Use From implementation to convert gRPC status to domain type
            let rpc_details = RpcStatusDetails::from(status);

            ServiceStatus {
                name: "RPC".to_string(),
                status: "healthy".to_string(),
                last_checked: current_time,
                error: None,
                details: Some(ServiceDetails {
                    rpc_status: Some(rpc_details),
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

pub async fn check_remote_prover_status(
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

pub async fn check_status(
    shared_status: SharedStatus,
    config: MonitoringConfig,
) -> anyhow::Result<()> {
    let mut rpc = ClientBuilder::new(config.rpc_url.clone())
        .without_tls()
        .without_timeout()
        .without_metadata_version()
        .without_metadata_genesis()
        .connect_lazy::<Rpc>();

    // Create remote prover clients for each URL
    let mut remote_provers: Vec<(
        String,
        String,
        miden_node_proto::clients::RemoteProverProxyClient,
    )> = config
        .remote_prover_urls
        .iter()
        .enumerate()
        .map(|(i, url)| {
            let name = format!("Prover-{}", i + 1);
            let url_str = url.to_string();
            let client = ClientBuilder::new(url.clone())
                .without_tls()
                .without_timeout()
                .without_metadata_version()
                .without_metadata_genesis()
                .connect_lazy::<RemoteProverProxy>();
            (name, url_str, client)
        })
        .collect();

    loop {
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("Failed to get current time")?
            .as_secs();

        let mut services = Vec::new();

        // Check RPC status
        let rpc_status = check_rpc_status(&mut rpc, current_time).await;
        services.push(rpc_status);

        // Check each Remote Prover status
        for (name, url, remote_prover) in &mut remote_provers {
            let remote_prover_status =
                check_remote_prover_status(remote_prover, name.clone(), url.clone(), current_time)
                    .await;
            services.push(remote_prover_status);
        }

        // Update shared status
        {
            let mut status = shared_status.lock().await;
            status.services = services;
            status.last_updated = current_time;
        }

        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}
