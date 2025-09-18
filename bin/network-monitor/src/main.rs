use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use miden_node_utils::logging::OpenTelemetry;
use tokio::sync::Mutex;
use tokio::task::JoinSet;

mod frontend;
mod status;

use frontend::serve;
use status::{MonitorConfig, NetworkStatus, SharedStatus, check_status};
use tracing::info;

/// Component identifier for structured logging and tracing
pub const COMPONENT: &str = "miden-network-monitor";

// MAIN
// ================================================================================================

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

    // Initialize shared status state
    let shared_status: SharedStatus =
        Arc::new(Mutex::new(NetworkStatus { services: Vec::new(), last_updated: 0 }));

    // Start tasks for frontend and status monitoring
    let mut join_set = JoinSet::new();
    let mut component_ids = HashMap::new();

    // Spawn frontend task
    let frontend_status = shared_status.clone();
    let frontend_config = config.clone();
    let id = join_set
        .spawn(async move { serve(frontend_status, frontend_config).await })
        .id();
    component_ids.insert(id, "frontend");

    // Spawn status monitoring task
    let status_status = shared_status.clone();
    let status_config = config.clone();
    let id = join_set
        .spawn(async move { check_status(status_status, status_config).await })
        .id();
    component_ids.insert(id, "status");

    // Wait for any task to complete or fail
    let component_result = join_set.join_next_with_id().await.expect("join set is not empty");

    // We expect components to run indefinitely, so we treat any return as fatal.
    let (id, err) = match component_result {
        Ok((id, Ok(_))) => (
            id,
            Err::<(), anyhow::Error>(anyhow::anyhow!("component completed unexpectedly")),
        ), // SAFETY: The JoinSet is definitely not empty.
        Ok((id, Err(err))) => (id, Err(err)), // SAFETY: The JoinSet is definitely not empty.
        Err(join_err) => (join_err.id(), Err(join_err).context("joining component task")),
    };
    let component = component_ids.get(&id).unwrap_or(&"unknown"); // SAFETY: The JoinSet is definitely not empty.

    // Exit with error context
    err.context(format!("component {component} failed"))
}
