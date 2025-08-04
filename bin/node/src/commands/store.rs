use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Context;
use miden_node_store::{
    Store,
    genesis::config::{AccountFileWithName, GenesisConfig},
};
use miden_node_utils::grpc::UrlExt;
use url::Url;

use super::{
    ENV_DATA_DIRECTORY, ENV_STORE_BLOCK_PRODUCER_URL, ENV_STORE_NTX_BUILDER_URL, ENV_STORE_RPC_URL,
};
use crate::commands::{
    DEFAULT_TIMEOUT, ENV_ENABLE_OTEL, ENV_GENESIS_CONFIG_FILE, duration_to_human_readable_string,
};

#[allow(clippy::large_enum_variant, reason = "single use enum")]
#[derive(clap::Subcommand)]
pub enum StoreCommand {
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
        /// Directory to write the account data to.
        #[arg(long, value_name = "DIR")]
        accounts_directory: PathBuf,
        /// Use the given configuration file to construct the genesis state from.
        #[arg(long, env = ENV_GENESIS_CONFIG_FILE, value_name = "GENESIS_CONFIG")]
        genesis_config_file: Option<PathBuf>,
    },

    /// Starts the store component.
    ///
    /// The store exposes three separate APIs, each on a different address and with the necessary
    /// endpoints to be accessed by the node's components.
    Start {
        /// Url at which to serve the store's RPC API.
        #[arg(long = "rpc.url", env = ENV_STORE_RPC_URL, value_name = "URL")]
        rpc_url: Url,

        /// Url at which to serve the store's network transaction builder API.
        #[arg(long = "ntx-builder.url", env = ENV_STORE_NTX_BUILDER_URL, value_name = "URL")]
        ntx_builder_url: Url,

        /// Url at which to serve the store's block producer API.
        #[arg(long = "block-producer.url", env = ENV_STORE_BLOCK_PRODUCER_URL, value_name = "URL")]
        block_producer_url: Url,

        /// Directory in which to store the database and raw block data.
        #[arg(long, env = ENV_DATA_DIRECTORY, value_name = "DIR")]
        data_directory: PathBuf,

        /// Enables the exporting of traces for OpenTelemetry.
        ///
        /// This can be further configured using environment variables as defined in the official
        /// OpenTelemetry documentation. See our operator manual for further details.
        #[arg(long = "enable-otel", default_value_t = false, env = ENV_ENABLE_OTEL, value_name = "BOOL")]
        enable_otel: bool,

        /// Timeout of the requests.
        #[arg(
            long = "timeout",
            default_value = &duration_to_human_readable_string(DEFAULT_TIMEOUT),
            value_parser = humantime::parse_duration,
            value_name = "DURATION"
        )]
        timeout: Duration,
    },
}

impl StoreCommand {
    /// Executes the subcommand as described by each variants documentation.
    pub async fn handle(self) -> anyhow::Result<()> {
        match self {
            StoreCommand::Bootstrap {
                data_directory,
                accounts_directory,
                genesis_config_file,
            } => {
                Self::bootstrap(&data_directory, &accounts_directory, genesis_config_file.as_ref())
            },
            StoreCommand::Start {
                rpc_url,
                ntx_builder_url,
                block_producer_url,
                data_directory,
                enable_otel: _,
                timeout,
            } => {
                Self::start(rpc_url, ntx_builder_url, block_producer_url, data_directory, timeout)
                    .await
            },
        }
    }

    pub fn is_open_telemetry_enabled(&self) -> bool {
        if let Self::Start { enable_otel, .. } = self {
            *enable_otel
        } else {
            false
        }
    }

    async fn start(
        rpc_url: Url,
        ntx_builder_url: Url,
        block_producer_url: Url,
        data_directory: PathBuf,
        timeout: Duration,
    ) -> anyhow::Result<()> {
        let rpc_listener = rpc_url
            .to_socket()
            .context("Failed to extract socket address from store RPC URL")?;
        let rpc_listener = tokio::net::TcpListener::bind(rpc_listener)
            .await
            .context("Failed to bind to store's RPC gRPC URL")?;

        let ntx_builder_addr = ntx_builder_url
            .to_socket()
            .context("Failed to extract socket address from store ntx-builder URL")?;
        let ntx_builder_listener = tokio::net::TcpListener::bind(ntx_builder_addr)
            .await
            .context("Failed to bind to store's ntx-builder gRPC URL")?;

        let block_producer_listener = block_producer_url
            .to_socket()
            .context("Failed to extract socket address from store block-producer URL")?;
        let block_producer_listener = tokio::net::TcpListener::bind(block_producer_listener)
            .await
            .context("Failed to bind to store's block-producer gRPC URL")?;

        Store {
            rpc_listener,
            ntx_builder_listener,
            block_producer_listener,
            data_directory,
            timeout,
        }
        .serve()
        .await
        .context("failed while serving store component")
    }

    fn bootstrap(
        data_directory: &Path,
        accounts_directory: &Path,
        maybe_genesis_config: Option<&PathBuf>,
    ) -> anyhow::Result<()> {
        let config = maybe_genesis_config
            .map(|genesis_config| {
                let toml_str = fs_err::read_to_string(genesis_config)?;
                let config = GenesisConfig::read_toml(toml_str.as_str())
                    .context(format!("Read from file: {}", genesis_config.display()))?;
                Ok::<_, anyhow::Error>(config)
            })
            .transpose()?
            .unwrap_or_default();

        let (genesis_state, secrets) = config.into_state()?;
        // Write the accounts to disk
        for item in secrets.as_account_files(&genesis_state) {
            let AccountFileWithName { account_file, name } = item?;
            let accountpath = accounts_directory.join(name);
            // do not override existing keys
            fs_err::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&accountpath)
                .context("key file already exists")?;
            account_file.write(accountpath)?;
        }

        Store::bootstrap(genesis_state, data_directory)
    }
}
