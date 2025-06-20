use std::time::Duration;

use anyhow::Context;
use miden_node_rpc::Rpc;
use miden_node_utils::grpc::UrlExt;
use url::Url;

use super::{
    DEFAULT_MONITOR_INTERVAL_MS, ENV_BLOCK_PRODUCER_URL, ENV_ENABLE_OTEL, ENV_RPC_URL,
    ENV_STORE_URL, parse_duration_ms,
};
use crate::system_monitor::SystemMonitor;

#[derive(clap::Subcommand)]
pub enum RpcCommand {
    /// Starts the RPC component.
    Start {
        /// Url at which to serve the gRPC API.
        #[arg(long = "url", env = ENV_RPC_URL, value_name = "URL")]
        url: Url,

        /// The store's gRPC url.
        #[arg(long = "store.url", env = ENV_STORE_URL, value_name = "URL")]
        store_url: Url,

        /// The block-producer's gRPC url. If unset, will run the RPC in read-only mode,
        /// i.e. without a block-producer.
        #[arg(long = "block-producer.url", env = ENV_BLOCK_PRODUCER_URL, value_name = "URL")]
        block_producer_url: Option<Url>,

        /// Enables the exporting of traces for OpenTelemetry.
        ///
        /// This can be further configured using environment variables as defined in the official
        /// OpenTelemetry documentation. See our operator manual for further details.
        #[arg(long = "enable-otel", default_value_t = false, env = ENV_ENABLE_OTEL, value_name = "BOOL")]
        open_telemetry: bool,

        /// Interval at which to monitor the system in milliseconds.
        #[arg(
            long = "monitor.interval",
            default_value = DEFAULT_MONITOR_INTERVAL_MS,
            value_parser = parse_duration_ms,
            value_name = "MILLISECONDS"
        )]
        monitor_interval: Duration,
    },
}

impl RpcCommand {
    pub async fn handle(self) -> anyhow::Result<()> {
        let Self::Start {
            url,
            store_url,
            block_producer_url,
            // Note: open-telemetry is handled in main.
            open_telemetry: _,
            monitor_interval,
        } = self;

        let store = store_url
            .to_socket()
            .context("Failed to extract socket address from store URL")?;

        let block_producer = if let Some(url) = block_producer_url {
            Some(url.to_socket().context("Failed to extract socket address from store URL")?)
        } else {
            None
        };

        let listener = url.to_socket().context("Failed to extract socket address from RPC URL")?;
        let listener = tokio::net::TcpListener::bind(listener)
            .await
            .context("Failed to bind to RPC's gRPC URL")?;

        // Start system monitor.
        SystemMonitor::new(monitor_interval).run_with_supervisor();

        Rpc { listener, store, block_producer }.serve().await.context("Serving RPC")
    }

    pub fn is_open_telemetry_enabled(&self) -> bool {
        let Self::Start { open_telemetry, .. } = self;
        *open_telemetry
    }
}
