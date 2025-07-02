use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use miden_lib::{AuthScheme, account::faucets::create_basic_fungible_faucet, utils::Serializable};
use miden_node_store::{GenesisState, Store, genesis::config::GenesisConfig};
use miden_node_utils::{crypto::get_rpo_random_coin, grpc::UrlExt};
use miden_objects::{
    Felt, ONE,
    account::{Account, AccountFile, AuthSecretKey},
    asset::TokenSymbol,
    crypto::dsa::rpo_falcon512::SecretKey,
};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use url::Url;

use super::{
    ENV_DATA_DIRECTORY, ENV_STORE_BLOCK_PRODUCER_URL, ENV_STORE_NTX_BUILDER_URL, ENV_STORE_RPC_URL,
};

/// The default filepath for the genesis account.
const DEFAULT_ACCOUNT_PATH: &str = "account.mac";

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
    },
}

impl StoreCommand {
    /// Executes the subcommand as described by each variants documentation.
    pub async fn handle(self) -> anyhow::Result<()> {
        match self {
<<<<<<< HEAD
            StoreCommand::Bootstrap { data_directory, accounts_directory } => {
                Self::bootstrap(&data_directory, &accounts_directory)
            },
            StoreCommand::Start {
                rpc_url,
                ntx_builder_url,
                block_producer_url,
                data_directory,
                enable_otel: _,
            } => Self::start(rpc_url, ntx_builder_url, block_producer_url, data_directory).await,
||||||| parent of fa62ab7 (integrate into bootstrap command and doc)
            StoreCommand::Bootstrap { data_directory, accounts_directory } => {
                Self::bootstrap(&data_directory, &accounts_directory)
            },
            StoreCommand::Start { url, data_directory, enable_otel: _ } => {
                Self::start(url, data_directory).await
            },
=======
            StoreCommand::Bootstrap {
                data_directory,
                accounts_directory,
                genesis_config_file: config_path,
            } => Self::bootstrap(&data_directory, &accounts_directory, config_path.as_ref()),
            StoreCommand::Start { url, data_directory, enable_otel: _ } => {
                Self::start(url, data_directory).await
            },
>>>>>>> fa62ab7 (integrate into bootstrap command and doc)
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
        }
        .serve()
        .await
        .context("failed while serving store component")
    }

    fn default_genesis_state(accounts_directory: &Path) -> anyhow::Result<GenesisState> {
        // Generate the genesis accounts.
        let account_file =
            Self::generate_genesis_account().context("failed to create genesis account")?;

        // Write account data to disk (including secrets).
        //
        // Without this the accounts would be inaccessible by the user.
        // This is not used directly by the node, but rather by the owner / operator of the
        // node.
        let filepath = accounts_directory.join(DEFAULT_ACCOUNT_PATH);
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
        Ok(GenesisState::new(vec![account_file.account], version, timestamp))
    }

    fn bootstrap(
        data_directory: &Path,
        accounts_directory: &Path,
        maybe_genesis_config_path: Option<&PathBuf>,
    ) -> anyhow::Result<()> {
        let genesis_state = if let Some(genesis_config_path) = maybe_genesis_config_path {
            let toml_str = fs_err::read_to_string(genesis_config_path)?;
            let config = GenesisConfig::read_toml(toml_str.as_str())
                .context(format!("Read from file: {}", genesis_config_path.display()))?;

            let (state, secrets) = config.into_state()?;
            // Write the accounts to disk
            for account in secrets.as_account_files() {
                let accountpath = accounts_directory.join(DEFAULT_ACCOUNT_PATH);
                account.write(accountpath)?;
            }
            state
        } else {
            Self::default_genesis_state(accounts_directory)?
        };
        Store::bootstrap(genesis_state, data_directory)
    }

    fn generate_genesis_account() -> anyhow::Result<AccountFile> {
        let mut rng = ChaCha20Rng::from_seed(rand::random());
        let secret = SecretKey::with_rng(&mut get_rpo_random_coin(&mut rng));

        // Calculate the max supply of the token.
        let decimals = 6u8;
        let base_unit = 10u64.pow(u32::from(decimals));
        let max_supply = 100_000_000_000u64 * base_unit;
        let max_supply = Felt::try_from(max_supply).expect("max supply is less than field modulus");

        // Create the faucet.
        let (account, account_seed) = create_basic_fungible_faucet(
            rng.random(),
            TokenSymbol::try_from("MIDEN").expect("MIDEN is a valid token symbol"),
            decimals,
            max_supply,
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
        let (id, vault, sorage, code, _) = account.into_parts();
        let updated_account = Account::from_parts(id, vault, sorage, code, ONE);

        Ok(AccountFile::new(
            updated_account,
            Some(account_seed),
            vec![AuthSecretKey::RpoFalcon512(secret)],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::StoreCommand;

    #[test]
    fn generate_genesis_account_no_panic() {
        let _account = StoreCommand::generate_genesis_account().unwrap();
    }
}
