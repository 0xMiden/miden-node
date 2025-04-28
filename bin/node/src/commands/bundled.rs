use std::{collections::HashMap, path::PathBuf, time::Duration};

use anyhow::Context;
use miden_node_block_producer::BlockProducer;
use miden_node_rpc::Rpc;
use miden_node_store::Store;
use miden_node_utils::grpc::UrlExt;
use tokio::{net::TcpListener, task::JoinSet};
use url::Url;

use super::{
    DEFAULT_BATCH_INTERVAL_MS, DEFAULT_BLOCK_INTERVAL_MS, ENV_BATCH_PROVER_URL,
    ENV_BLOCK_PROVER_URL, ENV_DATA_DIRECTORY, ENV_ENABLE_OTEL, ENV_RPC_URL, parse_duration_ms,
};

#[derive(clap::Subcommand)]
#[expect(clippy::large_enum_variant, reason = "This is a single use enum")]
pub enum BundledCommand {
    /// Bootstraps the blockchain database with the genesis block.
    ///
    /// The genesis block contains a single public faucet account. The private key for this
    /// account is written to the `accounts-directory` which can be used to control the account.
    ///
    /// This key is not required by the node and can be moved.
    Bootstrap {
        /// Directory in which to store the database and raw block data.
        #[arg(long, env = ENV_DATA_DIRECTORY, value_name = "DIR")]
        data_directory: PathBuf,
        // Directory to write the account data to.
        #[arg(long, value_name = "DIR")]
        accounts_directory: PathBuf,
    },

    /// Runs all three node components in the same process.
    ///
    /// The internal gRPC endpoints for the store and block-producer will each be assigned a random
    /// open port on localhost (127.0.0.1:0).
    Start {
        /// Url at which to serve the RPC component's gRPC API.
        #[arg(long = "rpc.url", env = ENV_RPC_URL, value_name = "URL")]
        rpc_url: Url,

        /// Directory in which the Store component should store the database and raw block data.
        #[arg(long = "data-directory", env = ENV_DATA_DIRECTORY, value_name = "DIR")]
        data_directory: PathBuf,

        /// The remote batch prover's gRPC url. If unset, will default to running a prover
        /// in-process which is expensive.
        #[arg(long = "batch-prover.url", env = ENV_BATCH_PROVER_URL, value_name = "URL")]
        batch_prover_url: Option<Url>,

        /// The remote block prover's gRPC url. If unset, will default to running a prover
        /// in-process which is expensive.
        #[arg(long = "block-prover.url", env = ENV_BLOCK_PROVER_URL, value_name = "URL")]
        block_prover_url: Option<Url>,

        /// Enables the exporting of traces for OpenTelemetry.
        ///
        /// This can be further configured using environment variables as defined in the official
        /// OpenTelemetry documentation. See our operator manual for further details.
        #[arg(long = "enable-otel", default_value_t = false, env = ENV_ENABLE_OTEL, value_name = "bool")]
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
    },
}

impl BundledCommand {
    pub async fn handle(self) -> anyhow::Result<()> {
        match self {
            BundledCommand::Bootstrap { data_directory, accounts_directory } => {
                // Currently the bundled bootstrap is identical to the store's bootstrap.
                crate::commands::store::StoreCommand::Bootstrap {
                    data_directory,
                    accounts_directory,
                }
                .handle()
                .await
                .context("failed to bootstrap the store component")
            },
            BundledCommand::Start {
                rpc_url,
                data_directory,
                batch_prover_url,
                block_prover_url,
                // Note: open-telemetry is handled in main.
                open_telemetry: _,
                block_interval,
                batch_interval,
            } => {
                Self::start(
                    rpc_url,
                    data_directory,
                    batch_prover_url,
                    block_prover_url,
                    batch_interval,
                    block_interval,
                )
                .await
            },
        }
    }

    async fn start(
        rpc_url: Url,
        data_directory: PathBuf,
        batch_prover_url: Option<Url>,
        block_prover_url: Option<Url>,
        batch_interval: Duration,
        block_interval: Duration,
    ) -> anyhow::Result<()> {
        // Start listening on all gRPC urls so that inter-component connections can be created
        // before each component is fully started up.
        //
        // This is required because `tonic` does not handle retries nor reconnections and our
        // services expect to be able to connect on startup.
        let grpc_rpc = rpc_url.to_socket().context("Failed to to RPC gRPC socket")?;
        let grpc_rpc = TcpListener::bind(grpc_rpc)
            .await
            .context("Failed to bind to RPC gRPC endpoint")?;
        let grpc_store = TcpListener::bind("127.0.0.1:0")
            .await
            .context("Failed to bind to store gRPC endpoint")?;
        let store_address =
            grpc_store.local_addr().context("Failed to retrieve the store's gRPC address")?;

        let block_producer_address = {
            let grpc_block_producer = TcpListener::bind("127.0.0.1:0")
                .await
                .context("Failed to bind to block-producer gRPC endpoint")?;
            grpc_block_producer
                .local_addr()
                .context("Failed to retrieve the block-producer's gRPC address")?
        };

        let mut join_set = JoinSet::new();

        // Start store. The store endpoint is available after loading completes.
        let store_id = join_set
            .spawn(async move {
                Store { listener: grpc_store, data_directory }
                    .serve()
                    .await
                    .context("failed while serving store component")
            })
            .id();

        // Start block-producer. The block-producer's endpoint is available after loading completes.
        let block_producer_id = join_set
            .spawn(async move {
                BlockProducer {
                    block_producer_address,
                    store_address,
                    batch_prover: batch_prover_url,
                    block_prover: block_prover_url,
                    batch_interval,
                    block_interval,
                }
                .serve()
                .await
                .context("failed while serving block-producer component")
            })
            .id();

        // Start RPC component.
        let rpc_id = join_set
            .spawn(async move {
                Rpc {
                    listener: grpc_rpc,
                    store: store_address,
                    block_producer: block_producer_address,
                }
                .serve()
                .await
                .context("failed while serving RPC component")
            })
            .id();

        // Lookup table so we can identify the failed component.
        let component_ids = HashMap::from([
            (store_id, "store"),
            (block_producer_id, "block-producer"),
            (rpc_id, "rpc"),
        ]);

        // SAFETY: The joinset is definitely not empty.
        let component_result = join_set.join_next_with_id().await.unwrap();

        // We expect components to run indefinitely, so we treat any return as fatal.
        //
        // Map all outcomes to an error, and provide component context.
        let (id, err) = match component_result {
            Ok((id, Ok(_))) => (id, Err(anyhow::anyhow!("Component completed unexpectedly"))),
            Ok((id, Err(err))) => (id, Err(err)),
            Err(join_err) => (join_err.id(), Err(join_err).context("Joining component task")),
        };
        let component = component_ids.get(&id).unwrap_or(&"unknown");

        // We could abort and gracefully shutdown the other components, but since we're crashing the
        // node there is no point.

        err.context(format!("Component {component} failed"))
    }

    pub fn is_open_telemetry_enabled(&self) -> bool {
        if let Self::Start { open_telemetry, .. } = self {
            *open_telemetry
        } else {
            false
        }
    }
}
