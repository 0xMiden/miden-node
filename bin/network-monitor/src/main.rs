use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use tokio::sync::Mutex;
use tokio::task::JoinSet;

mod frontend;
mod status;

use frontend::serve;
use status::{MonitorConfig, NetworkStatus, SharedStatus, check_status};

#[tokio::main]
async fn main() {
    // Load configuration from environment variables
    let config = match MonitorConfig::from_env() {
        Ok(config) => {
            println!("Loaded configuration:");
            println!("  RPC URL: {}", config.rpc_url);
            println!(
                "  Remote Prover URLs: {}",
                config
                    .remote_prover_urls
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            println!("  Port: {}", config.port);
            config
        },
        Err(e) => {
            eprintln!("Failed to load configuration: {e}");
            std::process::exit(1);
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
    let component_result = join_set.join_next_with_id().await.unwrap();

    // We expect components to run indefinitely, so we treat any return as fatal.
    let (id, err) = match component_result {
        Ok((id, Ok(_))) => (
            id,
            Err::<(), anyhow::Error>(anyhow::anyhow!("Component completed unexpectedly")),
        ), // SAFETY: The JoinSet is definitely not empty.
        Ok((id, Err(err))) => (id, Err(err)), // SAFETY: The JoinSet is definitely not empty.
        Err(join_err) => (join_err.id(), Err(join_err).context("Joining component task")),
    };
    let component = component_ids.get(&id).unwrap_or(&"unknown");

    // Exit with error context
    err.context(format!("Component {component} failed")).unwrap();
}
