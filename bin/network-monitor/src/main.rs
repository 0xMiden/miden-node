use std::collections::HashMap;

use anyhow::Context;
use miden_node_utils::logging::OpenTelemetry;
use tokio::sync::watch;
use tokio::task::JoinSet;

mod frontend;
mod status;

use frontend::{ServerState, serve};
use status::{MonitorConfig, ServiceStatus, run_remote_prover_status_task, run_rpc_status_task};
use tracing::info;

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
    miden_node_utils::logging::setup_tracing(OpenTelemetry::Disabled)?;

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

    let mut tasks = JoinSet::new();
    let mut component_ids: HashMap<_, String> = HashMap::new();

    // Create initial empty status for services
    let initial_rpc_status = ServiceStatus {
        name: "RPC".to_string(),
        status: "unknown".to_string(),
        last_checked: 0,
        error: None,
        details: None,
    };

    // Spawn the RPC checker
    let (rpc_tx, rpc_rx) = watch::channel(initial_rpc_status);
    let rpc_url = config.rpc_url.clone();
    let id = tasks.spawn(async move { run_rpc_status_task(rpc_url, rpc_tx).await }).id();
    component_ids.insert(id, "rpc-checker".to_string());

    // Spawn the prover checkers
    let mut prover_rxs = Vec::new();
    for (i, prover_url) in config.remote_prover_urls.iter().enumerate() {
        let name = format!("Prover-{}", i + 1);
        let initial_prover_status = ServiceStatus {
            name: format!("Remote Prover ({name})"),
            status: "unknown".to_string(),
            last_checked: 0,
            error: None,
            details: None,
        };

        let (prover_tx, prover_rx) = watch::channel(initial_prover_status);
        prover_rxs.push(prover_rx);

        let prover_url_clone = prover_url.clone();
        let name_clone = name.clone();
        let component_name = format!("prover-checker-{}", i + 1);
        let id = tasks
            .spawn(async move {
                run_remote_prover_status_task(prover_url_clone, name_clone, prover_tx).await
            })
            .id();
        component_ids.insert(id, component_name);
    }

    // Create http server
    let server_state = ServerState { rpc: rpc_rx, provers: prover_rxs };

    let server_config = config.clone();
    let id = tasks.spawn(async move { serve(server_state, server_config).await }).id();
    component_ids.insert(id, "frontend".to_string());

    // Wait for any task to complete or fail
    let component_result = tasks.join_next_with_id().await.expect("join set is not empty");

    // We expect components to run indefinitely, so we treat any return as fatal.
    let (id, err) = match component_result {
        Ok((id, Ok(_))) => (
            id,
            Err::<(), anyhow::Error>(anyhow::anyhow!("component completed unexpectedly")),
        ),
        Ok((id, Err(err))) => (id, Err(err)),
        Err(join_err) => (join_err.id(), Err(join_err).context("joining component task")),
    };
    let component_name = component_ids.get(&id).map_or("unknown", String::as_str);

    // Exit with error context
    err.context(format!("component {component_name} failed"))
}
