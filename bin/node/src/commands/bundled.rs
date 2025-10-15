use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use miden_node_block_producer::BlockProducer;
use miden_node_ntx_builder::NetworkTransactionBuilder;
use miden_node_rpc::Rpc;
use miden_node_store::Store;
use miden_node_utils::grpc::UrlExt;
use miden_node_validator::Validator;
use tokio::net::TcpListener;
use tokio::sync::Barrier;
use tokio::task::JoinSet;
use url::Url;

use super::{ENV_DATA_DIRECTORY, ENV_RPC_URL};
use crate::commands::{
    BlockProducerConfig,
    DEFAULT_TIMEOUT,
    ENV_ENABLE_OTEL,
    ENV_GENESIS_CONFIG_FILE,
    NtxBuilderConfig,
    duration_to_human_readable_string,
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
        /// Constructs the genesis block from the given toml file.
        #[arg(long, env = ENV_GENESIS_CONFIG_FILE, value_name = "FILE")]
        genesis_config_file: Option<PathBuf>,
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

        #[command(flatten)]
        block_producer: BlockProducerConfig,

        #[command(flatten)]
        ntx_builder: NtxBuilderConfig,

        /// Enables the exporting of traces for OpenTelemetry.
        ///
        /// This can be further configured using environment variables as defined in the official
        /// OpenTelemetry documentation. See our operator manual for further details.
        #[arg(long = "enable-otel", default_value_t = false, env = ENV_ENABLE_OTEL, value_name = "BOOL")]
        enable_otel: bool,

        /// Maximum duration a gRPC request is allocated before being dropped by the server.
        ///
        /// This may occur if the server is overloaded or due to an internal bug.
        #[arg(
            long = "grpc.timeout",
            default_value = &duration_to_human_readable_string(DEFAULT_TIMEOUT),
            value_parser = humantime::parse_duration,
            value_name = "DURATION"
        )]
        grpc_timeout: Duration,
    },
}

impl BundledCommand {
    pub async fn handle(self) -> anyhow::Result<()> {
        match self {
            BundledCommand::Bootstrap {
                data_directory,
                accounts_directory,
                genesis_config_file,
            } => {
                // Currently the bundled bootstrap is identical to the store's bootstrap.
                crate::commands::store::StoreCommand::Bootstrap {
                    data_directory,
                    accounts_directory,
                    genesis_config_file,
                }
                .handle()
                .await
                .context("failed to bootstrap the store component")
            },
            BundledCommand::Start {
                rpc_url,
                data_directory,
                block_producer,
                ntx_builder,
                enable_otel: _,
                grpc_timeout,
            } => {
                Self::start(rpc_url, data_directory, ntx_builder, block_producer, grpc_timeout)
                    .await
            },
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn start(
        rpc_url: Url,
        data_directory: PathBuf,
        ntx_builder: NtxBuilderConfig,
        block_producer: BlockProducerConfig,
        grpc_timeout: Duration,
    ) -> anyhow::Result<()> {
        let should_start_ntb = !ntx_builder.disabled;
        // Start listening on all gRPC urls so that inter-component connections can be created
        // before each component is fully started up.
        //
        // This is required because `tonic` does not handle retries nor reconnections and our
        // services expect to be able to connect on startup.
        let grpc_rpc = rpc_url.to_socket().context("Failed to to RPC gRPC socket")?;
        let grpc_rpc = TcpListener::bind(grpc_rpc)
            .await
            .context("Failed to bind to RPC gRPC endpoint")?;

        let block_producer_address = TcpListener::bind("127.0.0.1:0")
            .await
            .context("Failed to bind to block-producer gRPC endpoint")?
            .local_addr()
            .context("Failed to retrieve the block-producer's gRPC address")?;

        let validator_address = TcpListener::bind("127.0.0.1:0")
            .await
            .context("Failed to bind to validator gRPC endpoint")?
            .local_addr()
            .context("Failed to retrieve the validator's gRPC address")?;

        // Store addresses for each exposed API
        let store_rpc_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("Failed to bind to store RPC gRPC endpoint")?;
        let store_ntx_builder_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("Failed to bind to store ntx-builder gRPC endpoint")?;
        let store_block_producer_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("Failed to bind to store block-producer gRPC endpoint")?;
        let store_rpc_address = store_rpc_listener
            .local_addr()
            .context("Failed to retrieve the store's RPC gRPC address")?;
        let store_block_producer_address = store_block_producer_listener
            .local_addr()
            .context("Failed to retrieve the store's block-producer gRPC address")?;
        let store_ntx_builder_address = store_ntx_builder_listener
            .local_addr()
            .context("Failed to retrieve the store's ntx-builder gRPC address")?;

        let mut join_set = JoinSet::new();
        // Start store. The store endpoint is available after loading completes.
        let data_directory_clone = data_directory.clone();
        let store_id = join_set
            .spawn(async move {
                Store {
                    rpc_listener: store_rpc_listener,
                    block_producer_listener: store_block_producer_listener,
                    ntx_builder_listener: store_ntx_builder_listener,
                    data_directory: data_directory_clone,
                    grpc_timeout,
                }
                .serve()
                .await
                .context("failed while serving store component")
            })
            .id();

        // A sync point between the ntb and block-producer components.
        let checkpoint = if should_start_ntb {
            Barrier::new(2)
        } else {
            Barrier::new(1)
        };
        let checkpoint = Arc::new(checkpoint);

        // Start block-producer. The block-producer's endpoint is available after loading completes.
        let block_producer_id = join_set
            .spawn({
                let checkpoint = Arc::clone(&checkpoint);
                let store_url = Url::parse(&format!("http://{store_block_producer_address}"))
                    .context("Failed to parse URL")?;
                async move {
                    BlockProducer {
                        block_producer_address,
                        store_url,
                        batch_prover_url: block_producer.batch_prover_url,
                        block_prover_url: block_producer.block_prover_url,
                        batch_interval: block_producer.batch_interval,
                        block_interval: block_producer.block_interval,
                        max_batches_per_block: block_producer.max_batches_per_block,
                        max_txs_per_batch: block_producer.max_txs_per_batch,
                        production_checkpoint: checkpoint,
                        grpc_timeout,
                    }
                    .serve()
                    .await
                    .context("failed while serving block-producer component")
                }
            })
            .id();

        let validator_id = join_set
            .spawn({
                async move {
                    Validator {
                        address: validator_address,
                        grpc_timeout,
                        data_directory,
                    }
                    .serve()
                    .await
                    .context("failed while serving validator component")
                }
            })
            .id();

        // Start RPC component.
        let rpc_id = join_set
            .spawn(async move {
                let store_url = Url::parse(&format!("http://{store_rpc_address}"))
                    .context("Failed to parse URL")?;
                let block_producer_url = Url::parse(&format!("http://{block_producer_address}"))
                    .context("Failed to parse URL")?;
                Rpc {
                    listener: grpc_rpc,
                    store_url,
                    block_producer_url: Some(block_producer_url),
                    grpc_timeout,
                }
                .serve()
                .await
                .context("failed while serving RPC component")
            })
            .id();

        // Lookup table so we can identify the failed component.
        let mut component_ids = HashMap::from([
            (store_id, "store"),
            (block_producer_id, "block-producer"),
            (validator_id, "validator"),
            (rpc_id, "rpc"),
        ]);

        // Start network transaction builder. The endpoint is available after loading completes.
        let store_ntx_builder_url = Url::parse(&format!("http://{store_ntx_builder_address}"))
            .context("Failed to parse URL")?;

        if should_start_ntb {
            let id = join_set
                .spawn(async move {
                    let block_producer_url =
                        Url::parse(&format!("http://{block_producer_address}"))
                            .context("Failed to parse URL")?;
                    NetworkTransactionBuilder {
                        store_url: store_ntx_builder_url,
                        block_producer_url,
                        tx_prover_url: ntx_builder.tx_prover_url,
                        ticker_interval: ntx_builder.ticker_interval,
                        bp_checkpoint: checkpoint,
                    }
                    .serve_new()
                    .await
                    .context("failed while serving ntx builder component")
                })
                .id();
            component_ids.insert(id, "ntx-builder");
        }

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
        if let Self::Start { enable_otel, .. } = self {
            *enable_otel
        } else {
            false
        }
    }
}
