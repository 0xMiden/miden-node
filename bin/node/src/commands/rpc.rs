use anyhow::Context;
use miden_node_utils::grpc::UrlExt;
use url::Url;

use super::{ENV_BLOCK_PRODUCER_URL, ENV_ENABLE_OTEL, ENV_RPC_URL, ENV_STORE_URL};

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

        /// The block-producer's gRPC url.
        #[arg(long = "block-producer.url", env = ENV_BLOCK_PRODUCER_URL, value_name = "URL")]
        block_producer_url: Url,

        /// Enables the exporting of traces for OpenTelemetry.
        ///
        /// This can be further configured using environment variables as defined in the official
        /// OpenTelemetry documentation. See our operator manual for further details.
        #[arg(long = "enable-otel", default_value_t = false, env = ENV_ENABLE_OTEL, value_name = "bool")]
        open_telemetry: bool,
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
        } = self;

        let store_url = store_url
            .to_socket()
            .context("Failed to extract socket address from store URL")?;
        let block_producer_url = block_producer_url
            .to_socket()
            .context("Failed to extract socket address from store URL")?;

        let listener = url.to_socket().context("Failed to extract socket address from RPC URL")?;
        let listener = tokio::net::TcpListener::bind(listener)
            .await
            .context("Failed to bind to RPC's gRPC URL")?;

        miden_node_rpc::serve(listener, store_url, block_producer_url)
            .await
            .context("Serving RPC")
    }

    pub fn is_open_telemetry_enabled(&self) -> bool {
        let Self::Start { open_telemetry, .. } = self;
        *open_telemetry
    }
}
