use anyhow::Context;
use miden_node_block_producer::BlockProducer;
use miden_node_utils::grpc::UrlExt;
use tokio::time::Duration;
use url::Url;

use super::{
    DEFAULT_BATCH_INTERVAL_MS, DEFAULT_BLOCK_INTERVAL_MS, DEFAULT_MONITOR_INTERVAL_MS,
    ENV_BATCH_PROVER_URL, ENV_BLOCK_PRODUCER_URL, ENV_BLOCK_PROVER_URL, ENV_ENABLE_OTEL,
    ENV_STORE_URL, parse_duration_ms,
};
use crate::system_monitor::SystemMonitor;

#[derive(clap::Subcommand)]
pub enum BlockProducerCommand {
    /// Starts the block-producer component.
    Start {
        /// Url at which to serve the gRPC API.
        #[arg(env = ENV_BLOCK_PRODUCER_URL)]
        url: Url,

        /// The store's gRPC url.
        #[arg(long = "store.url", env = ENV_STORE_URL)]
        store_url: Url,

        /// The remote batch prover's gRPC url. If unset, will default to running a prover
        /// in-process which is expensive.
        #[arg(long = "batch-prover.url", env = ENV_BATCH_PROVER_URL)]
        batch_prover_url: Option<Url>,

        /// The remote block prover's gRPC url. If unset, will default to running a prover
        /// in-process which is expensive.
        #[arg(long = "block-prover.url", env = ENV_BLOCK_PROVER_URL)]
        block_prover_url: Option<Url>,

        /// Enables the exporting of traces for OpenTelemetry.
        ///
        /// This can be further configured using environment variables as defined in the official
        /// OpenTelemetry documentation. See our operator manual for further details.
        #[arg(long = "enable-otel", default_value_t = false, env = ENV_ENABLE_OTEL)]
        open_telemetry: bool,

        /// Interval at which to produce blocks in milliseconds.
        #[arg(
            long = "block.interval",
            default_value = DEFAULT_BLOCK_INTERVAL_MS,
            value_parser = parse_duration_ms,
            value_name = "MILLISECONDS"
        )]
        block_interval: Duration,

        /// Interval at which to procude batches in milliseconds.
        #[arg(
            long = "batch.interval",
            default_value = DEFAULT_BATCH_INTERVAL_MS,
            value_parser = parse_duration_ms,
            value_name = "MILLISECONDS"
        )]
        batch_interval: Duration,

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

impl BlockProducerCommand {
    pub async fn handle(self) -> anyhow::Result<()> {
        let Self::Start {
            url,
            store_url,
            batch_prover_url,
            block_prover_url,
            // Note: open-telemetry is handled in main.
            open_telemetry: _,
            block_interval,
            batch_interval,
            monitor_interval,
        } = self;

        let store_address = store_url
            .to_socket()
            .context("Failed to extract socket address from store URL")?;

        let block_producer_address =
            url.to_socket().context("Failed to extract socket address from store URL")?;

        // Start system monitor.
        SystemMonitor::new(monitor_interval).run_with_supervisor();

        BlockProducer {
            block_producer_address,
            store_address,
            batch_prover_url,
            block_prover_url,
            batch_interval,
            block_interval,
        }
        .serve()
        .await
        .context("failed while serving block-producer component")
    }

    pub fn is_open_telemetry_enabled(&self) -> bool {
        let Self::Start { open_telemetry, .. } = self;
        *open_telemetry
    }
}
