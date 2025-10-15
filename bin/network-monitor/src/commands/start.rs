//! Start command implementation.
//!
//! This module contains the implementation for starting the network monitoring service.

use anyhow::Result;
use miden_node_utils::logging::OpenTelemetry;
use tracing::{info, warn};

use crate::config::MonitorConfig;
use crate::deploy::ensure_counter_exist;
use crate::frontend::ServerState;
use crate::monitor::tasks::Tasks;

/// Start the network monitoring service.
///
/// This function initializes all monitoring tasks including RPC status checking,
/// remote prover testing, faucet testing, and the web frontend.
pub async fn start_monitor(config: MonitorConfig) -> Result<()> {
    // Load configuration from command-line arguments and environment variables
    info!("Loaded configuration: {:?}", config);

    if config.enable_otel {
        miden_node_utils::logging::setup_tracing(OpenTelemetry::Enabled)?;
    } else {
        miden_node_utils::logging::setup_tracing(OpenTelemetry::Disabled)?;
    }

    let mut tasks = Tasks::new();

    // Initialize the RPC Status endpoint checker task.
    let rpc_rx = tasks.spawn_rpc_checker(&config).await?;

    // Initialize the prover checkers & tests tasks, only if URLs were provided.
    let prover_rxs = if config.remote_prover_urls.is_empty() {
        Vec::new()
    } else {
        tasks.spawn_prover_tasks(&config).await?
    };

    // Initialize the faucet testing task.
    let faucet_rx = if config.faucet_url.is_some() {
        Some(tasks.spawn_faucet(&config))
    } else {
        warn!("Faucet URL not configured, skipping faucet testing");
        None
    };

    // Initialize the counter increment task only if enabled.
    let counter_rx = if config.enable_counter {
        // Ensure counter account exist before starting monitoring tasks
        Box::pin(ensure_counter_exist(&config.counter_file, &config.rpc_url)).await?;

        Some(tasks.spawn_counter_increment(&config))
    } else {
        None
    };

    // Initialize HTTP server.
    let server_state = ServerState {
        rpc: rpc_rx,
        provers: prover_rxs,
        faucet: faucet_rx,
        counter: counter_rx,
    };
    tasks.spawn_http_server(server_state, &config);

    tasks.handle_failure().await
}
