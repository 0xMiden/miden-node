use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use miden_lib::{AuthScheme, account::faucets::create_basic_fungible_faucet, utils::Serializable};
use miden_node_store::{DataDirectory, GenesisState, Store};
use miden_node_utils::{crypto::get_rpo_random_coin, grpc::UrlExt};
use miden_objects::{
    Felt, ONE,
    account::{AccountFile, AccountIdAnchor, AuthSecretKey},
    asset::TokenSymbol,
    crypto::dsa::rpo_falcon512::SecretKey,
};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use url::Url;

use super::{
    DEFAULT_MONITOR_INTERVAL_MS, ENV_DATA_DIRECTORY, ENV_ENABLE_OTEL, ENV_STORE_URL,
    parse_duration_ms,
};
use crate::system_monitor::SystemMonitor;

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
        // Directory to write the account data to.
        #[arg(long, value_name = "DIR")]
        accounts_directory: PathBuf,
    },

    /// Starts the store component.
    Start {
        /// Url at which to serve the gRPC API.
        #[arg(long, env = ENV_STORE_URL, value_name = "URL")]
        url: Url,

        /// Directory in which to store the database and raw block data.
        #[arg(long, env = ENV_DATA_DIRECTORY, value_name = "DIR")]
        data_directory: PathBuf,

        /// Enables the exporting of traces for OpenTelemetry.
        ///
        /// This can be further configured using environment variables as defined in the official
        /// OpenTelemetry documentation. See our operator manual for further details.
        #[arg(long = "enable-otel", default_value_t = false, env = ENV_ENABLE_OTEL, value_name = "bool")]
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

impl StoreCommand {
    /// Executes the subcommand as described by each variants documentation.
    pub async fn handle(self) -> anyhow::Result<()> {
        match self {
            StoreCommand::Bootstrap { data_directory, accounts_directory } => {
                Self::bootstrap(&data_directory, &accounts_directory)
            },
            // Note: open-telemetry is handled in main.
            StoreCommand::Start {
                url,
                data_directory,
                open_telemetry: _,
                monitor_interval,
            } => Self::start(url, data_directory, monitor_interval).await,
        }
    }

    pub fn is_open_telemetry_enabled(&self) -> bool {
        if let Self::Start { open_telemetry, .. } = self {
            *open_telemetry
        } else {
            false
        }
    }

    async fn start(
        url: Url,
        data_directory: PathBuf,
        monitor_interval: Duration,
    ) -> anyhow::Result<()> {
        let listener =
            url.to_socket().context("Failed to extract socket address from store URL")?;
        let listener = tokio::net::TcpListener::bind(listener)
            .await
            .context("Failed to bind to store's gRPC URL")?;

        // Start system monitor.
        let data_dir =
            DataDirectory::load(data_directory.clone()).context("failed to load data directory")?;
        SystemMonitor::new(monitor_interval)
            .with_store_metrics(data_dir)
            .run_with_supervisor();

        Store { listener, data_directory }
            .serve()
            .await
            .context("failed while serving store component")
    }

    fn bootstrap(data_directory: &Path, accounts_directory: &Path) -> anyhow::Result<()> {
        // Generate the genesis accounts.
        let account_file =
            Self::generate_genesis_account().context("failed to create genesis account")?;

        // Write account data to disk (including secrets).
        //
        // Without this the accounts would be inaccessible by the user.
        // This is not used directly by the node, but rather by the owner / operator of the node.
        let filepath = accounts_directory.join("account.mac");
        File::create_new(&filepath)
            .and_then(|mut file| file.write_all(&account_file.to_bytes()))
            .with_context(|| {
                format!("failed to write data for genesis account to file {}", filepath.display())
            })?;

        // Bootstrap the store database.
        let version = 1;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current timestamp should be greater than unix epoch")
            .as_secs()
            .try_into()
            .expect("timestamp should fit into u32");
        let genesis_state = GenesisState::new(vec![account_file.account], version, timestamp);
        Store::bootstrap(genesis_state, data_directory)
    }

    fn generate_genesis_account() -> anyhow::Result<AccountFile> {
        let mut rng = ChaCha20Rng::from_seed(rand::random());
        let secret = SecretKey::with_rng(&mut get_rpo_random_coin(&mut rng));

        let (mut account, account_seed) = create_basic_fungible_faucet(
            rng.random(),
            AccountIdAnchor::PRE_GENESIS,
            TokenSymbol::try_from("POL").expect("POL should be a valid token symbol"),
            12,
            Felt::from(1_000_000u32),
            miden_objects::account::AccountStorageMode::Public,
            AuthScheme::RpoFalcon512 { pub_key: secret.public_key() },
        )?;

        // Force the account nonce to 1.
        //
        // By convention, a nonce of zero indicates a freshly generated local account that has yet
        // to be deployed. An account is deployed onchain along with its first transaction which
        // results in a non-zero nonce onchain.
        //
        // The genesis block is special in that accounts are "deplyed" without transactions and
        // therefore we need bump the nonce manually to uphold this invariant.
        account.set_nonce(ONE).context("failed to set account nonce to 1")?;

        Ok(AccountFile::new(
            account,
            Some(account_seed),
            AuthSecretKey::RpoFalcon512(secret),
        ))
    }
}
