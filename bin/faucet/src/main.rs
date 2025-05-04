mod config;
mod faucet;
mod rpc_client;
mod server;
mod types;

#[cfg(test)]
mod stub_rpc_api;

use std::{path::PathBuf, str::FromStr};

use anyhow::Context;
use clap::{Parser, Subcommand};
use faucet::Faucet;
use miden_lib::{AuthScheme, account::faucets::create_basic_fungible_faucet};
use miden_node_utils::{config::load_config, crypto::get_rpo_random_coin, version::LongVersion};
use miden_objects::{
    Felt,
    account::{AccountFile, AccountStorageMode, AuthSecretKey},
    asset::TokenSymbol,
    crypto::dsa::rpo_falcon512::SecretKey,
};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use rpc_client::RpcClient;
use server::Server;
use tokio::sync::mpsc;
use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};

use crate::config::{DEFAULT_FAUCET_ACCOUNT_PATH, FaucetConfig};

// CONSTANTS
// =================================================================================================

const COMPONENT: &str = "miden-faucet";
const FAUCET_CONFIG_FILE_PATH: &str = "miden-faucet.toml";

// COMMANDS
// ================================================================================================

#[derive(Parser)]
#[command(version, about, long_about = None, long_version = long_version().to_string())]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Start the faucet server
    Start {
        #[arg(short, long, value_name = "FILE", default_value = FAUCET_CONFIG_FILE_PATH)]
        config: PathBuf,
    },

    /// Create a new public faucet account and save to the specified file
    CreateFaucetAccount {
        #[arg(short, long, value_name = "FILE", default_value = FAUCET_CONFIG_FILE_PATH)]
        config_path: PathBuf,
        #[arg(short, long, value_name = "FILE", default_value = DEFAULT_FAUCET_ACCOUNT_PATH)]
        output_path: PathBuf,
        #[arg(short, long)]
        token_symbol: String,
        #[arg(short, long)]
        decimals: u8,
        #[arg(short, long)]
        max_supply: u64,
    },

    /// Generate default configuration file for the faucet
    Init {
        #[arg(short, long, default_value = FAUCET_CONFIG_FILE_PATH)]
        config_path: String,
        #[arg(short, long, default_value = DEFAULT_FAUCET_ACCOUNT_PATH)]
        faucet_account_path: String,
    },
}

// MAIN
// =================================================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    setup_tracing();

    let cli = Cli::parse();

    run_faucet_command(cli).await
}

async fn run_faucet_command(cli: Cli) -> anyhow::Result<()> {
    match &cli.command {
        Command::Start { config } => {
            let config: FaucetConfig =
                load_config(config).context("failed to load configuration file")?;

            let mut rpc_client = RpcClient::connect_lazy(config.node_url.clone())
                .context("failed to create RPC client")?;
            let account_file = AccountFile::read(&config.faucet_account_path)
                .context("failed to load faucet account from file")?;

            let faucet = Faucet::load(account_file, &mut rpc_client).await?;

            // Maximum of 100 requests in-queue at once. Overflow is rejected for faster feedback.
            let (tx_requests, rx_requests) = mpsc::channel(100);

            let server =
                Server::new(faucet.faucet_id(), config.asset_amount_options.clone(), tx_requests);

            // Run both the client and the server concurrently.
            // TODO: We probably want a more graceful shutdown.
            let faucet =
                async { faucet.run(rpc_client, rx_requests).await.context("faucet failed") };
            let server = async move { server.serve(config.port).await.context("server failed") };
            let mut tasks = tokio::task::JoinSet::new();
            tasks.spawn(faucet);
            tasks.spawn(server);
            // SAFETY: join_next returns None if there are no tasks, and we definitely have tasks.
            tasks
                .join_next()
                .await
                .unwrap()
                .context("failed to join task")?
                .context("a server task ended unexpectedly")?;
        },

        Command::CreateFaucetAccount {
            config_path,
            output_path,
            token_symbol,
            decimals,
            max_supply,
        } => {
            println!("Generating new faucet account. This may take a few minutes...");

            let config: FaucetConfig =
                load_config(config_path).context("failed to load configuration file")?;

            let mut rpc_client =
                RpcClient::connect_lazy(config.node_url).context("failed to create RPC client")?;

            let genesis_header = rpc_client
                .get_genesis_header()
                .await
                .context("fetching genesis header from the node")?;

            let current_dir =
                std::env::current_dir().context("failed to open current directory")?;

            let mut rng = ChaCha20Rng::from_seed(rand::random());

            let secret = SecretKey::with_rng(&mut get_rpo_random_coin(&mut rng));

            let (account, account_seed) = create_basic_fungible_faucet(
                rng.random(),
                (&genesis_header).try_into().context("failed to create anchor block")?,
                TokenSymbol::try_from(token_symbol.as_str())
                    .context("failed to parse token symbol")?,
                *decimals,
                Felt::try_from(*max_supply)
                    .expect("max supply value is greater than or equal to the field modulus"),
                AccountStorageMode::Public,
                AuthScheme::RpoFalcon512 { pub_key: secret.public_key() },
            )
            .context("failed to create basic fungible faucet account")?;

            let account_data =
                AccountFile::new(account, Some(account_seed), AuthSecretKey::RpoFalcon512(secret));

            let output_path = current_dir.join(output_path);
            account_data.write(&output_path).with_context(|| {
                format!("failed to write account data to file: {}", output_path.display())
            })?;

            println!("Faucet account file successfully created at: {}", output_path.display());
        },

        Command::Init { config_path, faucet_account_path } => {
            let current_dir =
                std::env::current_dir().context("failed to open current directory")?;

            let config_file_path = current_dir.join(config_path);

            let config = FaucetConfig {
                faucet_account_path: faucet_account_path.into(),
                ..FaucetConfig::default()
            };

            let config_as_toml_string =
                toml::to_string(&config).context("failed to serialize default config")?;

            std::fs::write(&config_file_path, config_as_toml_string)
                .context("error writing config to file")?;

            println!("Config file successfully created at: {config_file_path:?}");
        },
    }

    Ok(())
}

fn setup_tracing() {
    use tracing::level_filters::LevelFilter;
    use tracing_subscriber::{
        EnvFilter,
        filter::{FilterExt, Targets},
    };

    let fmt = tracing_subscriber::fmt::layer().pretty();

    let filter = match std::env::var(EnvFilter::DEFAULT_ENV) {
        Ok(rust_log) => FilterExt::boxed(
            EnvFilter::from_str(&rust_log)
                .expect("RUST_LOG should contain a valid filter configuration"),
        ),
        Err(std::env::VarError::NotUnicode(_)) => panic!("RUST_LOG contained non-unicode"),
        Err(std::env::VarError::NotPresent) => {
            FilterExt::boxed(
                Targets::new()
                    .with_default(LevelFilter::INFO)
                    // axum Extractor rejections are logged at TRACE and are useful for
                    // errors.
                    .with_target("axum::rejection", LevelFilter::TRACE)
                    // Prover and execution log a bunch at INFO level. Disable these.
                    .with_target("winter_prover", LevelFilter::WARN)
                    .with_target("miden_processor", LevelFilter::WARN)
                    .with_target("miden_prover", LevelFilter::WARN),
            )
        },
    };

    tracing_subscriber::registry().with(fmt.with_filter(filter)).init();
}

/// Generates [`LongVersion`] using the metadata generated by build.rs.
fn long_version() -> LongVersion {
    // Use optional to allow for build script embedding failure.
    LongVersion {
        version: env!("CARGO_PKG_VERSION"),
        sha: option_env!("VERGEN_GIT_SHA").unwrap_or_default(),
        branch: option_env!("VERGEN_GIT_BRANCH").unwrap_or_default(),
        dirty: option_env!("VERGEN_GIT_DIRTY").unwrap_or_default(),
        features: option_env!("VERGEN_CARGO_FEATURES").unwrap_or_default(),
        rust_version: option_env!("VERGEN_RUSTC_SEMVER").unwrap_or_default(),
        host: option_env!("VERGEN_RUSTC_HOST_TRIPLE").unwrap_or_default(),
        target: option_env!("VERGEN_CARGO_TARGET_TRIPLE").unwrap_or_default(),
        opt_level: option_env!("VERGEN_CARGO_OPT_LEVEL").unwrap_or_default(),
        debug: option_env!("VERGEN_CARGO_DEBUG").unwrap_or_default(),
    }
}

#[cfg(test)]
mod test {
    use std::{
        env::temp_dir,
        io::{BufRead, BufReader},
        process::{Command, Stdio},
        str::FromStr,
    };

    use fantoccini::ClientBuilder;
    use serde_json::{Map, json};
    use url::Url;

    use crate::{Cli, config::FaucetConfig, run_faucet_command, stub_rpc_api::serve_stub};

    /// This test starts a stub node, a faucet connected to the stub node, and a chromedriver
    /// to test the faucet website. It then loads the website and checks that all the requests
    /// made return status 200.
    #[tokio::test]
    async fn test_website() {
        let stub_node_url = Url::from_str("http://localhost:50051").unwrap();

        // Start the stub node
        tokio::spawn({
            let stub_node_url = stub_node_url.clone();
            async move { serve_stub(&stub_node_url).await.unwrap() }
        });

        let config_path = temp_dir().join("faucet.toml");
        let faucet_account_path = temp_dir().join("account.mac");

        // Create config
        let config = FaucetConfig {
            node_url: stub_node_url,
            faucet_account_path: faucet_account_path.clone(),
            ..FaucetConfig::default()
        };
        let config_as_toml_string = toml::to_string(&config).unwrap();
        std::fs::write(&config_path, config_as_toml_string).unwrap();

        // Create faucet account
        run_faucet_command(Cli {
            command: crate::Command::CreateFaucetAccount {
                config_path: config_path.clone(),
                output_path: faucet_account_path.clone(),
                token_symbol: "TEST".to_string(),
                decimals: 2,
                max_supply: 1000,
            },
        })
        .await
        .unwrap();

        // Start the faucet connected to the stub
        let website_url = format!("http://localhost:{}", config.port);
        tokio::spawn(async move {
            run_faucet_command(Cli {
                command: crate::Command::Start { config: config_path },
            })
            .await
            .unwrap();
        });

        // Start chromedriver. This requires having chromedriver and chrome installed
        let chromedriver_port = "57709";
        #[expect(clippy::zombie_processes)]
        let mut chromedriver = Command::new("chromedriver")
            .arg(format!("--port={chromedriver_port}"))
            .stdout(Stdio::piped())
            .spawn()
            .expect("failed to start chromedriver");
        // Wait for chromedriver to be running
        let stdout = chromedriver.stdout.take().unwrap();
        for line in BufReader::new(stdout).lines() {
            if line.unwrap().contains("ChromeDriver was started successfully") {
                break;
            }
        }

        // Start fantoccini client
        let client = ClientBuilder::native()
            .capabilities(
                [(
                    "goog:chromeOptions".to_string(),
                    json!({"args": ["--headless", "--disable-gpu", "--no-sandbox"]}),
                )]
                .into_iter()
                .collect::<Map<_, _>>(),
            )
            .connect(&format!("http://localhost:{chromedriver_port}"))
            .await
            .expect("failed to connect to WebDriver");

        // Open the website
        client.goto(website_url.as_str()).await.unwrap();

        let title = client.title().await.unwrap();
        assert_eq!(title, "Miden Faucet");

        // Execute a script to get all the failed requests
        let script = r"
            let errors = [];
            performance.getEntriesByType('resource').forEach(entry => {
                if (entry.responseStatus && entry.responseStatus >= 400) {
                    errors.push({url: entry.name, status: entry.responseStatus});
                }
            });
            return errors;
        ";
        let failed_requests = client.execute(script, vec![]).await.unwrap();
        assert!(failed_requests.as_array().unwrap().is_empty());

        // Close the client and kill chromedriver
        client.close().await.unwrap();
        chromedriver.kill().unwrap();
    }
}
