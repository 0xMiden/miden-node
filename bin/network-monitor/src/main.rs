use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
use tracing::{debug, info};

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
    // Load configuration from environment variables
    let config = match MonitorConfig::from_env() {
        Ok(config) => {
            info!("Loaded configuration: {:?}", config);
            config
        },
        Err(e) => {
            anyhow::bail!("failed to load configuration: {e}");
        },
    };

    if config.enable_otel {
        miden_node_utils::logging::setup_tracing(OpenTelemetry::Enabled)?;
    } else {
        miden_node_utils::logging::setup_tracing(OpenTelemetry::Disabled)?;
    }

    let mut tasks = JoinSet::new();
    let mut component_ids: HashMap<_, String> = HashMap::new();

    // Initialize the RPC Status endpoint checker task.
    let rpc_rx = initialize_rpc_status_checker(&config, &mut tasks, &mut component_ids).await;

    // Initialize the prover checkers & tests tasks.
    let prover_rxs = initialize_prover_tasks(&config, &mut tasks, &mut component_ids).await;

    // Initialize the faucet testing task.
    let faucet_rx = initialize_faucet_task(&config, &mut tasks, &mut component_ids);

    // Initialize HTTP server.
    initialize_http_server(&config, rpc_rx, prover_rxs, faucet_rx, &mut tasks, &mut component_ids);

    handle_failure(tasks, component_ids).await
}

// RPC CHECKER INITIALIZER
// ================================================================================================

/// Initializes the RPC status checker.
///
/// This function initializes the RPC status checker and returns a receiver for the RPC status.
///
/// # Arguments
///
/// * `config` - The configuration for the RPC service.
/// * `tasks` - The join set for the tasks.
/// * `component_ids` - The component IDs.
///
/// # Returns
///
/// A receiver for the RPC status.
pub(crate) async fn initialize_rpc_status_checker(
    config: &MonitorConfig,
    tasks: &mut JoinSet<()>,
    component_ids: &mut HashMap<Id, String>,
) -> Receiver<ServiceStatus> {
    // Create initial status for RPC service
    let mut rpc = ClientBuilder::new(config.rpc_url.clone())
        .with_tls()
        .unwrap()
        .with_timeout(Duration::from_secs(10))
        .without_metadata_version()
        .without_metadata_genesis()
        .connect_lazy::<Rpc>();

    let current_time = current_unix_timestamp_secs();

    let initial_rpc_status = check_rpc_status(&mut rpc, current_time).await;

    // Spawn the RPC checker
    let (rpc_tx, rpc_rx) = watch::channel(initial_rpc_status);
    let rpc_url = config.rpc_url.clone();
    let id = tasks.spawn(async move { run_rpc_status_task(rpc_url, rpc_tx).await }).id();
    component_ids.insert(id, "rpc-checker".to_string());
    rpc_rx
}

// PROVER TASKS INITIALIZER
// ================================================================================================

/// Initializes the prover tasks.
///
/// This function initializes the prover tasks and returns a vector of receivers for the prover
/// status.
///
/// # Arguments
///
/// * `config` - The configuration for the provers.
/// * `tasks` - The join set for the tasks.
/// * `component_ids` - The component IDs.
///
/// # Returns
///
/// A vector of receivers for the prover status.
async fn initialize_prover_tasks(
    config: &MonitorConfig,
    tasks: &mut JoinSet<()>,
    component_ids: &mut HashMap<Id, String>,
) -> Vec<(watch::Receiver<ServiceStatus>, watch::Receiver<ServiceStatus>)> {
    let mut prover_rxs = Vec::new();
    for (i, prover_url) in config.remote_prover_urls.clone().into_iter().enumerate() {
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

        let (prover_status_tx, prover_status_rx) = watch::channel(initial_prover_status.clone());

        // Spawn the remote prover status check task
        let component_name = format!("prover-checker-{}", i + 1);
        let id = tasks
            .spawn({
                let prover_url = prover_url.clone();
                let name = name.clone();

                run_remote_prover_status_task(prover_url, name, prover_status_tx)
            })
            .id();
        component_ids.insert(id, component_name);

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
            let (prover_test_tx, prover_test_rx) = watch::channel(initial_prover_status.clone());

            let id = tasks
                .spawn(async move {
                    Box::pin(run_remote_prover_test_task(
                        prover_url.clone(),
                        &name,
                        proof_type,
                        payload,
                        prover_test_tx,
                    ))
                    .await;
                })
                .id();
            let component_name = format!("prover-test-{}", i + 1);
            component_ids.insert(id, component_name);

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

    prover_rxs
}

// FAUCET TASK INITIALIZER
// ================================================================================================

/// Initializes the faucet testing task.
///
/// This function initializes the faucet testing task and returns a receiver for the faucet status.
///
/// # Arguments
///
/// * `config` - The configuration for the faucet service.
/// * `tasks` - The join set for the tasks.
/// * `component_ids` - The component IDs.
///
/// # Returns
///
/// An optional receiver for the faucet status.
fn initialize_faucet_task(
    config: &MonitorConfig,
    tasks: &mut JoinSet<()>,
    component_ids: &mut HashMap<Id, String>,
) -> Option<Receiver<ServiceStatus>> {
    let Some(faucet_url) = &config.faucet_url else {
        info!("Faucet URL not configured, skipping faucet testing");
        return None;
    };

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
    let faucet_url = faucet_url.clone();
    let id = tasks
        .spawn(async move { run_faucet_test_task(faucet_url, faucet_tx).await })
        .id();
    component_ids.insert(id, "faucet-test".to_string());

    Some(faucet_rx)
}

// HTTP SERVER INITIALIZER
// ================================================================================================

/// Initializes the HTTP server.
///
/// This function initializes the HTTP server and returns a receiver for the HTTP status.
///
/// # Arguments
///
/// * `config` - The configuration for the HTTP server.
/// * `rpc_rx` - The receiver for the RPC status.
/// * `prover_rxs` - The receivers for the prover status.
/// * `faucet_rx` - The optional receiver for the faucet status.
/// * `tasks` - The join set for the tasks.
/// * `component_ids` - The component IDs.
pub(crate) fn initialize_http_server(
    config: &MonitorConfig,
    rpc_rx: Receiver<ServiceStatus>,
    prover_rxs: Vec<(watch::Receiver<ServiceStatus>, watch::Receiver<ServiceStatus>)>,
    faucet_rx: Option<Receiver<ServiceStatus>>,
    tasks: &mut JoinSet<()>,
    component_ids: &mut HashMap<Id, String>,
) {
    let server_state = ServerState {
        rpc: rpc_rx,
        provers: prover_rxs,
        faucet: faucet_rx,
    };

    let server_config = config.clone();
    let id = tasks.spawn(async move { serve(server_state, server_config).await }).id();
    component_ids.insert(id, "frontend".to_string());
}

// HANDLE FAILURE
// ================================================================================================

/// Handles the failure of a task.
///
/// This function handles the failure of a task.
///
/// # Arguments
///
/// * `tasks` - The join set for the tasks.
/// * `component_ids` - The component IDs.
///
/// # Returns
///
/// An error if the task fails.
async fn handle_failure(
    mut tasks: JoinSet<()>,
    component_ids: HashMap<Id, String>,
) -> anyhow::Result<()> {
    // Wait for any task to complete or fail
    let component_result = tasks.join_next_with_id().await.expect("join set is not empty");

    // We expect components to run indefinitely, so we treat any return as fatal.
    let (id, err) = match component_result {
        Ok((id, ())) => (id, anyhow::anyhow!("component completed unexpectedly")),
        Err(join_err) => (join_err.id(), anyhow::Error::from(join_err)),
    };
    let component_name = component_ids.get(&id).map_or("unknown", String::as_str);

    // Exit with error context
    Err(err.context(format!("component {component_name} failed")))
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
