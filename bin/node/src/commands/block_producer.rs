use anyhow::Context;
use miden_node_block_producer::BlockProducer;
use miden_node_utils::grpc::UrlExt;
use url::Url;

use super::{ENV_BLOCK_PRODUCER_URL, ENV_NTX_BUILDER_URL, ENV_STORE_URL};
use crate::commands::{BlockProducerConfig, ENV_ENABLE_OTEL};

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

        /// Enables the exporting of traces for OpenTelemetry.
        ///
        /// This can be further configured using environment variables as defined in the official
        /// OpenTelemetry documentation. See our operator manual for further details.
        #[arg(long = "enable-otel", default_value_t = false, env = ENV_ENABLE_OTEL, value_name = "BOOL")]
        enable_otel: bool,
    },
}

impl BlockProducerCommand {
    pub async fn handle(self) -> anyhow::Result<()> {
        let Self::Start {
            url,
            store_url,
            ntx_builder_url,
            block_producer,
            enable_otel: _,
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

        BlockProducer {
            block_producer_address,
            store_address,
            ntx_builder_address,
            batch_prover_url: block_producer.batch_prover_url,
            block_prover_url: block_producer.block_prover_url,
            batch_interval: block_producer.batch_interval,
            block_interval: block_producer.block_interval,
            max_txs_per_batch: block_producer.max_txs_per_batch,
            max_batches_per_block: block_producer.max_batches_per_block,
        }
        .serve()
        .await
        .context("failed while serving block-producer component")
    }

    pub fn is_open_telemetry_enabled(&self) -> bool {
        let Self::Start { enable_otel, .. } = self;
        *enable_otel
    }
}
