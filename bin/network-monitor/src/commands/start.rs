//! Start command implementation.
//!
//! This module contains the implementation for starting the network monitoring service.

use anyhow::Result;
use miden_node_utils::logging::OpenTelemetry;
use tracing::{info, warn};

use crate::config::MonitorConfig;
use crate::deploy::ensure_accounts_exist;
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

    // Ensure accounts exist before starting monitoring tasks
    Box::pin(ensure_accounts_exist(
        &config.wallet_file,
        &config.counter_file,
        &config.rpc_url,
    ))
    .await?;

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
