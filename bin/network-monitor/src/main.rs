use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::Parser;
use miden_node_proto::clients::{Builder as ClientBuilder, RemoteProverProxy, Rpc};
use miden_node_utils::logging::OpenTelemetry;
use tokio::sync::watch;
use tokio::sync::watch::Receiver;
use tokio::task::{Id, JoinSet};

mod config;
mod faucet;
mod frontend;
mod remote_prover;
mod status;

use config::MonitorConfig;
use faucet::run_faucet_test_task;
use remote_prover::run_remote_prover_test_task;
use status::{ServiceStatus, run_remote_prover_status_task, run_rpc_status_task};
use tracing::{debug, info, warn};

use crate::frontend::{ServerState, serve};
use crate::remote_prover::{ProofType, generate_prover_test_payload};
use crate::status::{ServiceDetails, Status, check_remote_prover_status, check_rpc_status};

/// Component identifier for structured logging and tracing
pub const COMPONENT: &str = "miden-network-monitor";

// MAIN
// ================================================================================================

/// Network Monitor main function.
///
/// This implementation spawns independent status checker tasks for each service (RPC and remote
/// provers) that communicate via watch channels. Each task continuously monitors its service and
/// sends status updates through a `watch::Sender`. The web server holds all the `watch::Receiver`
/// ends and aggregates status on-demand when serving HTTP requests. If any task terminates
/// unexpectedly, the entire process exits.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load configuration from command-line arguments and environment variables
    let config = MonitorConfig::parse();
    info!("Loaded configuration: {:?}", config);

    let _otel_guard = if config.enable_otel {
        miden_node_utils::logging::setup_tracing(OpenTelemetry::Enabled)?
    } else {
        miden_node_utils::logging::setup_tracing(OpenTelemetry::Disabled)?
    };

    let mut tasks = Tasks::new();

    // Initialize the RPC Status endpoint checker task.
    let rpc_rx = tasks.spawn_rpc_checker(&config).await?;

    // Initialize the prover checkers & tests tasks.
    let prover_rxs = tasks.spawn_prover_tasks(&config).await?;

    // Initialize the faucet testing task.
    let faucet_rx = if config.faucet_url.is_some() {
        Some(tasks.spawn_faucet(&config))
    } else {
        warn!("Faucet URL not configured, skipping faucet testing");
        None
    };

    // Initialize HTTP server.
    let server_state = ServerState {
        rpc: rpc_rx,
        provers: prover_rxs,
        faucet: faucet_rx,
    };
    tasks.spawn_http_server(server_state, &config);

    tasks.handle_failure().await
}

// TASKS MANAGEMENT
// ================================================================================================

/// Task management structure that encapsulates `JoinSet` and component names.
#[derive(Default)]
struct Tasks {
    handles: JoinSet<()>,
    names: HashMap<Id, String>,
}

impl Tasks {
    /// Create a new Tasks instance.
    fn new() -> Self {
        Self {
            handles: JoinSet::new(),
            names: HashMap::new(),
        }
    }

    /// Spawn the RPC status checker task.
    async fn spawn_rpc_checker(
        &mut self,
        config: &MonitorConfig,
    ) -> anyhow::Result<Receiver<ServiceStatus>> {
        // Create initial status for RPC service
        let mut rpc = ClientBuilder::new(config.rpc_url.clone())
            .with_tls()
            .expect("TLS is enabled")
            .with_timeout(Duration::from_secs(10))
            .without_metadata_version()
            .without_metadata_genesis()
            .connect_lazy::<Rpc>();

        let current_time = current_unix_timestamp_secs();
        let initial_rpc_status = check_rpc_status(&mut rpc, current_time).await;

        // Spawn the RPC checker
        let (rpc_tx, rpc_rx) = watch::channel(initial_rpc_status);
        let rpc_url = config.rpc_url.clone();
        let status_check_interval = config.status_check_interval;
        let id = self
            .handles
            .spawn(async move {
                run_rpc_status_task(rpc_url, rpc_tx, status_check_interval).await;
            })
            .id();
        self.names.insert(id, "rpc-checker".to_string());

        Ok(rpc_rx)
    }

    /// Spawn prover status and test tasks for all configured provers.
    async fn spawn_prover_tasks(
        &mut self,
        config: &MonitorConfig,
    ) -> anyhow::Result<Vec<(watch::Receiver<ServiceStatus>, watch::Receiver<ServiceStatus>)>> {
        let mut prover_rxs = Vec::new();

        for (i, prover_url) in config.remote_prover_urls.iter().enumerate() {
            let name = format!("Prover-{}", i + 1);

            let mut remote_prover = ClientBuilder::new(prover_url.clone())
                .with_tls()
                .expect("TLS is enabled")
                .with_timeout(Duration::from_secs(10))
                .without_metadata_version()
                .without_metadata_genesis()
                .connect_lazy::<RemoteProverProxy>();

            let current_time = current_unix_timestamp_secs();

            let initial_prover_status = check_remote_prover_status(
                &mut remote_prover,
                name.clone(),
                prover_url.to_string(),
                current_time,
            )
            .await;

            let (prover_status_tx, prover_status_rx) =
                watch::channel(initial_prover_status.clone());

            // Spawn the remote prover status check task
            let component_name = format!("prover-checker-{}", i + 1);
            let prover_url_clone = prover_url.clone();
            let name_clone = name.clone();
            let status_check_interval = config.status_check_interval;
            let id = self
                .handles
                .spawn(async move {
                    run_remote_prover_status_task(
                        prover_url_clone,
                        name_clone,
                        prover_status_tx,
                        status_check_interval,
                    )
                    .await;
                })
                .id();
            self.names.insert(id, component_name);

            // Extract proof_type directly from the service status
            let proof_type = match &initial_prover_status.details {
                crate::status::ServiceDetails::RemoteProverStatus(details) => {
                    details.supported_proof_type.clone()
                },
                _ => unreachable!("This is for remote provers only"),
            };

            // Only spawn test tasks for transaction provers
            let prover_test_rx = if matches!(proof_type, ProofType::Transaction) {
                debug!("Starting transaction proof tests for prover: {}", name);
                let payload = generate_prover_test_payload().await;
                let (prover_test_tx, prover_test_rx) =
                    watch::channel(initial_prover_status.clone());

                let prover_url_clone = prover_url.clone();
                let name_clone = name.clone();
                let id = self
                    .handles
                    .spawn(async move {
                        Box::pin(run_remote_prover_test_task(
                            prover_url_clone,
                            &name_clone,
                            proof_type,
                            payload,
                            prover_test_tx,
                        ))
                        .await;
                    })
                    .id();
                let component_name = format!("prover-test-{}", i + 1);
                self.names.insert(id, component_name);

                prover_test_rx
            } else {
                debug!(
                    "Skipping prover tests for {} (supports {:?} proofs, only testing Transaction proofs)",
                    name, proof_type
                );
                // For non-transaction provers, create a dummy receiver with no test task
                let (_tx, rx) = watch::channel(initial_prover_status.clone());
                rx
            };

            prover_rxs.push((prover_status_rx, prover_test_rx));
        }

        Ok(prover_rxs)
    }

    /// Spawn the faucet testing task.
    fn spawn_faucet(&mut self, config: &MonitorConfig) -> Receiver<ServiceStatus> {
        let current_time = current_unix_timestamp_secs();

        // Create initial faucet test status
        let initial_faucet_status = ServiceStatus {
            name: "Faucet".to_string(),
            status: Status::Unknown,
            last_checked: current_time,
            error: None,
            details: ServiceDetails::FaucetTest(crate::faucet::FaucetTestDetails {
                test_duration_ms: 0,
                success_count: 0,
                failure_count: 0,
                last_tx_id: None,
                challenge_difficulty: None,
            }),
        };

        // Spawn the faucet testing task
        let (faucet_tx, faucet_rx) = watch::channel(initial_faucet_status);
        // SAFETY: config.faucet_url is Some
        let faucet_url = config.faucet_url.clone().unwrap();
        let faucet_test_interval = config.faucet_test_interval;
        let id = self
            .handles
            .spawn(async move {
                run_faucet_test_task(faucet_url, faucet_tx, faucet_test_interval).await;
            })
            .id();
        self.names.insert(id, "faucet-test".to_string());

        faucet_rx
    }

    /// Spawn the HTTP frontend server.
    fn spawn_http_server(&mut self, server_state: ServerState, config: &MonitorConfig) {
        let config = config.clone();
        let id = self.handles.spawn(async move { serve(server_state, config).await }).id();
        self.names.insert(id, "frontend".to_string());
    }

    /// Wait for any task to complete or fail and return the result.
    async fn join_next_with_id(&mut self) -> Option<Result<(Id, ()), tokio::task::JoinError>> {
        self.handles.join_next_with_id().await
    }

    /// Get the component name for a given task ID.
    fn get_component_name(&self, id: Id) -> Option<&String> {
        self.names.get(&id)
    }

    /// Handles the failure of a task.
    ///
    /// This method waits for any task to complete or fail and returns an error.
    /// Since we expect components to run indefinitely, any task completion is treated as fatal.
    ///
    /// # Returns
    ///
    /// An error if any task fails or completes unexpectedly.
    async fn handle_failure(&mut self) -> anyhow::Result<()> {
        // Wait for any task to complete or fail
        let component_result = self.join_next_with_id().await.expect("join set is not empty");

        // We expect components to run indefinitely, so we treat any return as fatal.
        let (id, err) = match component_result {
            Ok((id, ())) => (id, anyhow::anyhow!("component completed unexpectedly")),
            Err(join_err) => (join_err.id(), anyhow::Error::from(join_err)),
        };
        let component_name = self.get_component_name(id).map_or("unknown", String::as_str);

        // Exit with error context
        Err(err.context(format!("component {component_name} failed")))
    }
}

// HELPERS
// ================================================================================================

/// Gets the current Unix timestamp in seconds.
///
/// This function is infallible - if the system time is somehow before Unix epoch
/// (extremely unlikely), it returns 0.
pub fn current_unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))  // Fallback to 0 if before Unix epoch
        .as_secs()
}
