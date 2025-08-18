use std::time::Duration;

use clap::Parser;
use miden_remote_prover::COMPONENT;
use miden_remote_prover::api::ProofType;
use proxy::StartProxy;
use tracing::instrument;
use update_workers::{AddWorkers, RemoveWorkers, UpdateWorkers};
use worker::StartWorker;

pub mod proxy;
pub mod update_workers;
pub mod worker;

pub(crate) const PROXY_HOST: &str = "0.0.0.0";

#[derive(Debug, Parser)]
pub(crate) struct ProxyConfig {
    /// Interval at which the system polls for available workers to assign new
    /// tasks.
    #[arg(long, default_value = "20ms", env = "MRP_AVAILABLE_WORKERS_POLLING_INTERVAL", value_parser = humantime::parse_duration)]
    pub(crate) available_workers_polling_interval: Duration,
    /// Maximum time to establish a connection.
    #[arg(long, default_value = "10s", env = "MRP_CONNECTION_TIMEOUT", value_parser = humantime::parse_duration)]
    pub(crate) connection_timeout: Duration,
    /// Health check interval.
    #[arg(long, default_value = "10s", env = "MRP_HEALTH_CHECK_INTERVAL", value_parser = humantime::parse_duration)]
    pub(crate) health_check_interval: Duration,
    /// Maximum number of items in the queue.
    #[arg(long, default_value = "10", env = "MRP_MAX_QUEUE_ITEMS")]
    pub(crate) max_queue_items: usize,
    /// Maximum number of requests per second per IP address.
    #[arg(long, default_value = "5", env = "MRP_MAX_REQ_PER_SEC")]
    pub(crate) max_req_per_sec: isize,
    /// Maximum number of retries per request.
    #[arg(long, default_value = "1", env = "MRP_MAX_RETRIES_PER_REQUEST")]
    pub(crate) max_retries_per_request: usize,
    /// Metrics configurations.
    #[command(flatten)]
    pub(crate) metrics_config: MetricsConfig,
    /// Port of the proxy.
    #[arg(long, default_value = "8082", env = "MRP_PORT")]
    pub(crate) port: u16,
    /// Maximum time allowed for a request to complete. Once exceeded, the request is
    /// aborted.
    #[arg(long, default_value = "100s", env = "MRP_TIMEOUT", value_parser = humantime::parse_duration)]
    pub(crate) timeout: Duration,
    /// Control port.
    ///
    /// Port used to add and remove workers from the proxy.
    #[arg(long, default_value = "8083", env = "MRP_CONTROL_PORT")]
    pub(crate) control_port: u16,
    /// Supported proof type.
    ///
    /// The type of proof the proxy will handle. Only workers that support the same proof type
    /// will be able to connect to the proxy.
    #[arg(long, default_value = "transaction", env = "MRP_PROOF_TYPE")]
    pub(crate) proof_type: ProofType,
    /// Grace period before starting the final step of the graceful shutdown after
    /// signaling shutdown.
    #[arg(long, default_value = "20s", env = "MRP_GRACE_PERIOD", value_parser = humantime::parse_duration)]
    pub(crate) grace_period: std::time::Duration,
    /// Timeout of the final step for the graceful shutdown.
    #[arg(long, default_value = "5s", env = "MRP_GRACEFUL_SHUTDOWN_TIMEOUT", value_parser = humantime::parse_duration)]
    pub(crate) graceful_shutdown_timeout: std::time::Duration,
}

#[derive(Debug, Clone, clap::Parser)]
pub struct MetricsConfig {
    /// Port for Prometheus-compatible metrics
    /// If specified, metrics will be enabled on this port. If not specified, metrics will be
    /// disabled.
    #[arg(long, env = "MRP_METRICS_PORT")]
    pub metrics_port: Option<u16>,
}

/// Root CLI struct
#[derive(Parser, Debug)]
#[command(
    name = "miden-remote-prover",
    about = "A stand-alone service for proving Miden transactions.",
    version,
    rename_all = "kebab-case"
)]
pub struct Cli {
    #[command(subcommand)]
    action: Command,
}

/// CLI actions
#[derive(Debug, Parser)]
pub enum Command {
    /// Starts the workers with the configuration defined in the command.
    StartWorker(StartWorker),
    /// Starts the proxy.
    StartProxy(StartProxy),
    /// Adds workers to the proxy.
    ///
    /// This command will make a request to the proxy to add the specified workers.
    AddWorkers(AddWorkers),
    /// Removes workers from the proxy.
    ///
    /// This command will make a request to the proxy to remove the specified workers.
    RemoveWorkers(RemoveWorkers),
}

/// CLI entry point
impl Cli {
    #[instrument(target = COMPONENT, name = "cli.execute", skip_all, ret(level = "info"), err)]
    pub async fn execute(&self) -> anyhow::Result<()> {
        match &self.action {
            // For the `StartWorker` command, we need to create a new runtime and run the worker
            Command::StartWorker(worker_init) => worker_init.execute().await,
            Command::StartProxy(proxy_init) => proxy_init.execute().await,
            Command::AddWorkers(update_workers) => {
                let update_workers: UpdateWorkers = update_workers.clone().into();
                update_workers.execute().await
            },
            Command::RemoveWorkers(update_workers) => {
                let update_workers: UpdateWorkers = update_workers.clone().into();
                update_workers.execute().await
            },
        }
    }
}
