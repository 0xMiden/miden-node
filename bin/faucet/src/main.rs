mod config;
mod faucet;
mod rpc_client;
mod server;
mod types;

#[cfg(test)]
mod stub_rpc_api;

use std::{collections::BTreeSet, path::PathBuf};

use anyhow::Context;
use base64::{Engine, prelude::BASE64_STANDARD};
use clap::{Parser, Subcommand};
use faucet::Faucet;
use miden_lib::{AuthScheme, account::faucets::create_basic_fungible_faucet};
use miden_node_utils::{
    config::load_config, crypto::get_rpo_random_coin, logging::OpenTelemetry, version::LongVersion,
};
use miden_objects::{
    Felt,
    account::{AccountFile, AccountStorageMode, AuthSecretKey, NetworkId},
    asset::TokenSymbol,
    crypto::dsa::rpo_falcon512::SecretKey,
};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use rpc_client::RpcClient;
use server::Server;
use tokio::sync::mpsc;
use url::Url;

use crate::config::{DEFAULT_FAUCET_ACCOUNT_PATH, FaucetConfig};

// CONSTANTS
// =================================================================================================

const COMPONENT: &str = "miden-faucet";
const FAUCET_CONFIG_FILE_PATH: &str = "miden-faucet.toml";
const ENV_ENABLE_OTEL: &str = "MIDEN_FAUCET_ENABLE_OTEL";
pub const REQUESTS_QUEUE_SIZE: usize = 1000;
const DEFAULT_API_KEYS_COUNT: &str = "1";
const API_KEY_PREFIX: &str = "miden_faucet_";

// TODO: we should probably parse this from the config file
const NETWORK_ID: NetworkId = NetworkId::Testnet;
const EXPLORER_URL: &str = "https://testnet.midenscan.com";

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

        /// Enables the exporting of traces for OpenTelemetry.
        ///
        /// This can be further configured using environment variables as defined in the official
        /// OpenTelemetry documentation. See our operator manual for further details.
        #[arg(long = "enable-otel", default_value_t = false, env = ENV_ENABLE_OTEL)]
        open_telemetry: bool,
    },

    /// Create a new public faucet account and save to the specified file
    CreateFaucetAccount {
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
        #[arg(short, long, default_value = DEFAULT_API_KEYS_COUNT)]
        generated_api_keys_count: u8,
        #[arg(short, long)]
        node_url: Option<String>,
    },
}

impl Command {
    fn open_telemetry(&self) -> OpenTelemetry {
        if match *self {
            Command::Start { config: _, open_telemetry } => open_telemetry,
            _ => false,
        } {
            OpenTelemetry::Enabled
        } else {
            OpenTelemetry::Disabled
        }
    }
}

// MAIN
// =================================================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Configure tracing with optional OpenTelemetry exporting support.
    miden_node_utils::logging::setup_tracing(cli.command.open_telemetry())
        .context("failed to initialize logging")?;

    run_faucet_command(cli).await
}

async fn run_faucet_command(cli: Cli) -> anyhow::Result<()> {
    match &cli.command {
        // Note: open-telemetry is handled in main.
        Command::Start { config, open_telemetry: _ } => {
            let config: FaucetConfig =
                load_config(config).context("failed to load configuration file")?;

            let mut rpc_client = RpcClient::connect_lazy(&config.node_url, config.timeout_ms)
                .context("failed to create RPC client")?;
            let account_file = AccountFile::read(&config.faucet_account_path)
                .context("failed to load faucet account from file")?;

            let faucet =
                Faucet::load(account_file, &mut rpc_client, config.remote_tx_prover_url).await?;

            // Maximum of 1000 requests in-queue at once. Overflow is rejected for faster feedback.
            let (tx_requests, rx_requests) = mpsc::channel(REQUESTS_QUEUE_SIZE);

            let server = Server::new(
                faucet.faucet_id(),
                config.asset_amount_options.clone(),
                tx_requests,
                &config.pow_secret,
                BTreeSet::from_iter(config.api_keys),
            );

            // Capture in a variable to avoid moving into two branches
            let config_endpoint = config.endpoint;

            // Use select to concurrently:
            // - Run and wait for the faucet (on current thread)
            // - Run and wait for server (in a spawned task)
            let faucet_future = faucet.run(rpc_client, rx_requests);
            let server_future = async {
                let server_handle = tokio::spawn(async move {
                    server.serve(config_endpoint).await.context("server failed")
                });
                server_handle.await.context("failed to join server task")?
            };

            tokio::select! {
                server_result = server_future => {
                    // If server completes first, return its result
                    server_result.context("server failed")
                },
                faucet_result = faucet_future => {
                    // Faucet completed, return its result
                    faucet_result.context("faucet failed")
                }
            }?;
        },

        Command::CreateFaucetAccount {
            output_path,
            token_symbol,
            decimals,
            max_supply,
        } => {
            println!("Generating new faucet account. This may take a few minutes...");

            let current_dir =
                std::env::current_dir().context("failed to open current directory")?;

            let mut rng = ChaCha20Rng::from_seed(rand::random());

            let secret = SecretKey::with_rng(&mut get_rpo_random_coin(&mut rng));

            let (account, account_seed) = create_basic_fungible_faucet(
                rng.random(),
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

        Command::Init {
            config_path,
            faucet_account_path,
            generated_api_keys_count,
            node_url,
        } => {
            let current_dir =
                std::env::current_dir().context("failed to open current directory")?;

            let config_file_path = current_dir.join(config_path);

            let api_keys =
                (0..*generated_api_keys_count).map(|_| generate_api_key()).collect::<Vec<_>>();

            let mut config = FaucetConfig {
                faucet_account_path: faucet_account_path.into(),
                api_keys,
                ..FaucetConfig::default()
            };
            if let Some(url) = node_url {
                config.node_url = Url::parse(url).context("failed to parse node URL")?;
            }
            let config_as_toml_string =
                toml::to_string(&config).context("failed to serialize default config")?;

            std::fs::write(&config_file_path, config_as_toml_string)
                .context("error writing config to file")?;

            println!("Config file successfully created at: {}", config_file_path.display());
        },
    }

    Ok(())
}

/// Generates a random API key for the faucet.
/// The API key is a base64 encoded string with the prefix `miden_faucet_`.
fn generate_api_key() -> String {
    let mut rng = ChaCha20Rng::from_seed(rand::random());
    let mut api_key = [0u8; 32];
    rng.fill(&mut api_key);
    format!("{API_KEY_PREFIX}{}", BASE64_STANDARD.encode(api_key))
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
        process::Stdio,
        str::FromStr,
        time::{Duration, Instant},
    };

    use base64::{Engine, prelude::BASE64_STANDARD};
    use fantoccini::ClientBuilder;
    use miden_node_utils::grpc::UrlExt;
    use serde_json::{Map, json};
    use tokio::{io::AsyncBufReadExt, time::sleep};
    use url::Url;

    use crate::{
        API_KEY_PREFIX, Cli, config::FaucetConfig, run_faucet_command, stub_rpc_api::serve_stub,
    };

    /// This test starts a stub node, a faucet connected to the stub node, and a chromedriver
    /// to test the faucet website. It then loads the website and checks that all the requests
    /// made return status 200.
    #[tokio::test]
    async fn test_website() {
        let website_url = start_test_faucet().await;
        let client = start_fantoccini_client().await;

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

        // Verify all requests are successful
        assert!(failed_requests.as_array().unwrap().is_empty());

        // Inject JavaScript to capture sse events
        let capture_events_script = r"
            window.capturedEvents = [];
            const original = EventSource.prototype.addEventListener;
            EventSource.prototype.addEventListener = function(type, listener) {
                const wrappedListener = function(event) {
                    window.capturedEvents.push({
                        type: type,
                        data: event.data
                    });
                    return listener(event);
                };
                return original.call(this, type, wrappedListener);
            };
        ";
        client.execute(capture_events_script, vec![]).await.unwrap();

        // Fill in the account address
        client
            .find(fantoccini::Locator::Css("#account-address"))
            .await
            .unwrap()
            .send_keys("mtst1qrvhealccdyj7gqqqrlxl4n4f53uxwaw")
            .await
            .unwrap();

        // Select the first asset amount option
        client
            .find(fantoccini::Locator::Css("#asset-amount"))
            .await
            .unwrap()
            .click()
            .await
            .unwrap();
        client
            .find(fantoccini::Locator::Css("#asset-amount option"))
            .await
            .unwrap()
            .click()
            .await
            .unwrap();

        // Click the public note button
        client
            .find(fantoccini::Locator::Css("#button-public"))
            .await
            .unwrap()
            .click()
            .await
            .unwrap();

        // Poll until minting is complete. We wait 10s and then poll every 2s for a max of
        // 55 times (total 2 mins).
        sleep(Duration::from_secs(10)).await;
        let mut captured_events: Vec<serde_json::Value> = vec![];
        for _ in 0..55 {
            let events = client
                .execute("return window.capturedEvents;", vec![])
                .await
                .unwrap()
                .as_array()
                .unwrap()
                .clone();
            if events.iter().any(|event| event["type"] == "note") {
                captured_events = events;
                break;
            }
            sleep(Duration::from_secs(2)).await;
        }

        // Verify the received events
        assert!(!captured_events.is_empty(), "Took too long to capture any events");
        assert!(captured_events.iter().any(|event| event["type"] == "update"));
        let note_event = captured_events.iter().find(|event| event["type"] == "note").unwrap();
        let note_data: serde_json::Value =
            serde_json::from_str(note_event["data"].as_str().unwrap()).unwrap();
        assert!(note_data["note_id"].is_string());
        assert!(note_data["account_id"].is_string());
        assert!(note_data["transaction_id"].is_string());
        assert!(note_data["explorer_url"].is_string());

        client.close().await.unwrap();
    }

    async fn start_test_faucet() -> Url {
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
                output_path: faucet_account_path.clone(),
                token_symbol: "TEST".to_string(),
                decimals: 2,
                max_supply: 1000,
            },
        })
        .await
        .unwrap();

        // Start the faucet connected to the stub
        // Use std::thread to launch faucet - avoids Send requirements
        std::thread::spawn(move || {
            // Create a new runtime for this thread
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to build runtime");

            // Run the faucet on this thread's runtime
            rt.block_on(async {
                run_faucet_command(Cli {
                    command: crate::Command::Start {
                        config: config_path,
                        open_telemetry: false,
                    },
                })
                .await
                .unwrap();
            });
        });

        // Wait for faucet to be up
        let addr = config.endpoint.to_socket().unwrap();
        let start = Instant::now();
        let timeout = Duration::from_secs(10);
        loop {
            match tokio::net::TcpStream::connect(addr).await {
                Ok(_) => break,
                Err(_) if start.elapsed() < timeout => {
                    sleep(Duration::from_millis(200)).await;
                },
                Err(e) => panic!("faucet never became reachable: {e}"),
            }
        }

        config.endpoint
    }

    async fn start_fantoccini_client() -> fantoccini::Client {
        // Start chromedriver. This requires having chromedriver and chrome installed
        let chromedriver_port = "57708";
        let mut chromedriver = tokio::process::Command::new("chromedriver")
            .arg(format!("--port={chromedriver_port}"))
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("failed to start chromedriver");
        let stdout = chromedriver.stdout.take().unwrap();
        tokio::spawn(
            async move { chromedriver.wait().await.expect("chromedriver process failed") },
        );
        // Wait for chromedriver to be running
        let mut reader = tokio::io::BufReader::new(stdout).lines();
        while let Some(line) = reader.next_line().await.unwrap() {
            if line.contains("ChromeDriver was started successfully") {
                break;
            }
        }

        // Start fantoccini client
        ClientBuilder::native()
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
            .expect("failed to connect to WebDriver")
    }

    #[test]
    fn test_api_key_generation() {
        let api_key = crate::generate_api_key();
        assert!(api_key.starts_with(API_KEY_PREFIX));
        let decoded = BASE64_STANDARD.decode(&api_key.as_bytes()[API_KEY_PREFIX.len()..]).unwrap();
        assert_eq!(decoded.len(), 32);
    }
}
