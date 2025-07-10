use anyhow::Context;
use miden_node_rpc::Rpc;
use miden_node_utils::grpc::UrlExt;
use url::Url;

use super::{ENV_BLOCK_PRODUCER_URL, ENV_RPC_URL, ENV_STORE_RPC_URL};
use crate::commands::ENV_ENABLE_OTEL;

#[derive(clap::Subcommand)]
pub enum RpcCommand {
    /// Starts the RPC component.
    Start {
        /// Url at which to serve the gRPC API.
        #[arg(long = "url", env = ENV_RPC_URL, value_name = "URL")]
        url: Url,

        /// The store's RPC service gRPC url.
        #[arg(long = "store.url", env = ENV_STORE_RPC_URL, value_name = "URL")]
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
        enable_otel: bool,
    },
}

impl RpcCommand {
    pub async fn handle(self) -> anyhow::Result<()> {
        let Self::Start {
            url,
            store_url,
            block_producer_url,
            enable_otel: _,
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

        Rpc { listener, store, block_producer }.serve().await.context("Serving RPC")
    }

    pub fn is_open_telemetry_enabled(&self) -> bool {
        let Self::Start { enable_otel, .. } = self;
        *enable_otel
    }
}
