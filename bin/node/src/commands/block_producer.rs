use anyhow::Context;
use miden_node_block_producer::BlockProducer;
use miden_node_utils::grpc::UrlExt;
use url::Url;

use super::{
    BlockProducerConfig, ENV_BLOCK_PRODUCER_URL, ENV_NTX_BUILDER_URL, ENV_STORE_URL,
    TelemetryConfig,
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

        /// The network transaction builder's gRPC url.
        #[arg(long = "ntx-builder.url", env = ENV_NTX_BUILDER_URL)]
        ntx_builder_url: Option<Url>,

        #[command(flatten)]
        block_producer: BlockProducerConfig,

        #[command(flatten)]
        telemetry: TelemetryConfig,
    },
}

impl BlockProducerCommand {
    pub async fn handle(self) -> anyhow::Result<()> {
        let Self::Start {
            url,
            store_url,
            ntx_builder_url,
            block_producer,
            telemetry,
        } = self;

        let store_address = store_url
            .to_socket()
            .context("Failed to extract socket address from store URL")?;
        let ntx_builder_address = ntx_builder_url
            .map(|url| {
                url.to_socket().context(
                    "Failed to extract socket address from network transaction builder URL",
                )
            })
            .transpose()?;

        let block_producer_address =
            url.to_socket().context("Failed to extract socket address from store URL")?;

        // Start system monitor.
        SystemMonitor::new(telemetry.monitor_interval).run_with_supervisor();

        BlockProducer {
            block_producer_address,
            store_address,
            ntx_builder_address,
            batch_prover_url: block_producer.batch_prover_url,
            block_prover_url: block_producer.block_prover_url,
            batch_interval: block_producer.batch_interval,
            block_interval: block_producer.block_interval,
        }
        .serve()
        .await
        .context("failed while serving block-producer component")
    }

    pub fn is_open_telemetry_enabled(&self) -> bool {
        let Self::Start { telemetry, .. } = self;
        telemetry.open_telemetry
    }
}
