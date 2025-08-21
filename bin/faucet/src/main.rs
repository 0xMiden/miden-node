mod faucet;
mod rpc_client;
mod server;
mod types;

mod network;
#[cfg(test)]
mod stub_rpc_api;

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use faucet::Faucet;
use miden_lib::AuthScheme;
use miden_lib::account::faucets::create_basic_fungible_faucet;
use miden_node_utils::crypto::get_rpo_random_coin;
use miden_node_utils::logging::OpenTelemetry;
use miden_node_utils::version::LongVersion;
use miden_objects::Felt;
use miden_objects::account::{AccountFile, AccountStorageMode, AuthSecretKey};
use miden_objects::asset::TokenSymbol;
use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use rpc_client::RpcClient;
use server::Server;
use tokio::sync::mpsc;
use types::AssetOptions;
use url::Url;

use crate::network::FaucetNetwork;
use crate::server::{ApiKey, PoWConfig};

// CONSTANTS
// =================================================================================================

pub const REQUESTS_QUEUE_SIZE: usize = 1000;
const COMPONENT: &str = "miden-faucet";

const ENV_ENDPOINT: &str = "MIDEN_FAUCET_ENDPOINT";
const ENV_NODE_URL: &str = "MIDEN_FAUCET_NODE_URL";
const ENV_TIMEOUT: &str = "MIDEN_FAUCET_TIMEOUT";
const ENV_ACCOUNT_PATH: &str = "MIDEN_FAUCET_ACCOUNT_PATH";
const ENV_ASSET_AMOUNTS: &str = "MIDEN_FAUCET_ASSET_AMOUNTS";
const ENV_REMOTE_TX_PROVER_URL: &str = "MIDEN_FAUCET_REMOTE_TX_PROVER_URL";
const ENV_POW_SECRET: &str = "MIDEN_FAUCET_POW_SECRET";
const ENV_POW_CHALLENGE_LIFETIME: &str = "MIDEN_FAUCET_POW_CHALLENGE_LIFETIME";
const ENV_POW_CLEANUP_INTERVAL: &str = "MIDEN_FAUCET_POW_CLEANUP_INTERVAL";
const ENV_POW_GROWTH_RATE: &str = "MIDEN_FAUCET_POW_GROWTH_RATE";
const ENV_POW_BASELINE: &str = "MIDEN_FAUCET_POW_BASELINE";
const ENV_API_KEYS: &str = "MIDEN_FAUCET_API_KEYS";
const ENV_ENABLE_OTEL: &str = "MIDEN_FAUCET_ENABLE_OTEL";
const ENV_NETWORK: &str = "MIDEN_FAUCET_NETWORK";

// COMMANDS
// ================================================================================================

#[derive(Parser)]
#[command(version, about, long_about = None, long_version = long_version().to_string())]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
pub enum Command {
    /// Start the faucet server
    Start {
        /// Endpoint of the faucet in the format `<ip>:<port>`.
        #[arg(long = "endpoint", value_name = "URL", env = ENV_ENDPOINT)]
        endpoint: Url,

        /// Network configuration to use. Options are `devnet`, `testnet`, `localhost` or a custom
        /// network. It is used to show the correct addresses and explorer URL in the UI.
        #[arg(long = "network", value_name = "NETWORK", default_value = "localhost", env = ENV_NETWORK)]
        network: FaucetNetwork,

        /// Node RPC gRPC endpoint in the format `http://<host>[:<port>]`.
        #[arg(long = "node-url", value_name = "URL", env = ENV_NODE_URL)]
        node_url: Url,

        /// Timeout for RPC requests.
        #[arg(long = "timeout", value_name = "DURATION", default_value = "5s", env = ENV_TIMEOUT, value_parser = humantime::parse_duration)]
        timeout: Duration,

        /// Path to the faucet account file.
        #[arg(long = "account", value_name = "FILE", env = ENV_ACCOUNT_PATH)]
        faucet_account_path: PathBuf,

        /// Comma-separated list of amounts of asset that should be dispersed on each request.
        #[arg(long = "asset-amounts", value_name = "U64", env = ENV_ASSET_AMOUNTS, num_args = 1.., value_delimiter = ',', default_value = "100,500,1000")]
        asset_amounts: Vec<u64>,

        /// Endpoint of the remote transaction prover in the format `<protocol>://<host>[:<port>]`.
        #[arg(long = "remote-tx-prover-url", value_name = "URL", env = ENV_REMOTE_TX_PROVER_URL)]
        remote_tx_prover_url: Option<Url>,

        /// The secret to be used by the server to generate the `PoW` seed.
        #[arg(long = "pow-secret", value_name = "STRING", env = ENV_POW_SECRET)]
        pow_secret: Option<String>,

        /// The duration during which the `PoW` challenges are valid. Changing this will affect the
        /// rate limiting, since it works by rejecting new submissions while the previous submitted
        /// challenge is still valid.
        #[arg(long = "pow-challenge-lifetime", value_name = "DURATION", env = ENV_POW_CHALLENGE_LIFETIME, default_value = "30s", value_parser = humantime::parse_duration)]
        pow_challenge_lifetime: Duration,

        /// A measure of how quickly the `PoW` difficult grows with the number of requests. When
        /// set to 1, the difficulty will roughly double when the number of requests doubles.
        #[arg(long = "pow-growth-rate", value_name = "NON_ZERO_USIZE", env = ENV_POW_GROWTH_RATE, default_value = "1")]
        pow_growth_rate: NonZeroUsize,

        /// The interval at which the `PoW` challenge cache is cleaned up.
        #[arg(long = "pow-cleanup-interval", value_name = "DURATION", env = ENV_POW_CLEANUP_INTERVAL, default_value = "2s", value_parser = humantime::parse_duration)]
        pow_cleanup_interval: Duration,

        /// The baseline for the `PoW` challenges. This sets the `PoW` difficulty (in bits) that a
        /// a challenge will have when there are no requests against the faucet. It must be between
        /// 0 and 32.
        #[arg(value_parser = clap::value_parser!(u8).range(0..=32))]
        #[arg(long = "pow-baseline", value_name = "U8", env = ENV_POW_BASELINE, default_value = "12")]
        pow_baseline: u8,

        /// Comma-separated list of API keys.
        #[arg(long = "api-keys", value_name = "STRING", env = ENV_API_KEYS, num_args = 1.., value_delimiter = ',')]
        api_keys: Vec<String>,

        /// Enables the exporting of traces for OpenTelemetry.
        ///
        /// This can be further configured using environment variables as defined in the official
        /// OpenTelemetry documentation. See our operator manual for further details.
        #[arg(long = "enable-otel", value_name = "BOOL", default_value_t = false, env = ENV_ENABLE_OTEL)]
        open_telemetry: bool,
    },

    /// Create a new public faucet account and save to the specified file.
    CreateFaucetAccount {
        #[arg(short, long, value_name = "FILE")]
        output: PathBuf,
        #[arg(short, long, value_name = "STRING")]
        token_symbol: String,
        #[arg(short, long, value_name = "U8")]
        decimals: u8,
        #[arg(short, long, value_name = "U64")]
        max_supply: u64,
    },

    /// Generate API keys that can be used by the faucet.
    ///
    /// Prints out the specified number of API keys to stdout as a comma-separated list.
    /// This list can be supplied to the faucet via the `--api-keys` flag or `MIDEN_FAUCET_API_KEYS`
    /// env var of the start command.
    CreateApiKeys {
        #[arg()]
        count: u8,
    },
}

impl Command {
    fn open_telemetry(&self) -> OpenTelemetry {
        if match *self {
            Command::Start { open_telemetry, .. } => open_telemetry,
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

#[allow(clippy::too_many_lines)]
async fn run_faucet_command(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        // Note: open-telemetry is handled in main.
        Command::Start {
            endpoint,
            network,
            node_url,
            timeout,
            faucet_account_path,
            remote_tx_prover_url,
            asset_amounts,
            pow_secret,
            pow_challenge_lifetime,
            pow_cleanup_interval,
            pow_growth_rate,
            pow_baseline,
            api_keys,
            open_telemetry: _,
        } => {
            let mut rpc_client = RpcClient::connect_lazy(node_url, timeout.as_millis() as u64)
                .context("failed to create RPC client")?;
            let account_file = AccountFile::read(&faucet_account_path).context(format!(
                "failed to load faucet account from file ({})",
                faucet_account_path.display()
            ))?;

            let faucet = Faucet::load(
                network.to_network_id()?,
                account_file,
                &mut rpc_client,
                remote_tx_prover_url,
            )
            .await?;

            // Maximum of 1000 requests in-queue at once. Overflow is rejected for faster feedback.
            let (tx_requests, rx_requests) = mpsc::channel(REQUESTS_QUEUE_SIZE);

            let api_keys = api_keys
                .iter()
                .map(|k| ApiKey::decode(k))
                .collect::<Result<Vec<_>, _>>()
                .context("failed to decode API keys")?;
            let asset_options = AssetOptions::new(asset_amounts)
                .map_err(|e| anyhow::anyhow!("failed to create asset options: {}", e))?;
            let pow_config = PoWConfig {
                challenge_lifetime: pow_challenge_lifetime,
                cleanup_interval: pow_cleanup_interval,
                growth_rate: pow_growth_rate,
                baseline: pow_baseline,
            };
            let server = Server::new(
                faucet.faucet_id(),
                asset_options,
                tx_requests,
                pow_secret.unwrap_or_default().as_str(),
                pow_config,
                &api_keys,
            );

            // Use select to concurrently:
            // - Run and wait for the faucet (on current thread)
            // - Run and wait for server (in a spawned task)
            let faucet_future = faucet.run(rpc_client, rx_requests);
            let server_future = async {
                let server_handle =
                    tokio::spawn(
                        async move { server.serve(endpoint).await.context("server failed") },
                    );
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
            output: output_path,
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
                decimals,
                Felt::try_from(max_supply)
                    .expect("max supply value is greater than or equal to the field modulus"),
                AccountStorageMode::Public,
                AuthScheme::RpoFalcon512 { pub_key: secret.public_key() },
            )
            .context("failed to create basic fungible faucet account")?;

            let account_data = AccountFile::new(
                account,
                Some(account_seed),
                vec![AuthSecretKey::RpoFalcon512(secret)],
            );

            let output_path = current_dir.join(output_path);
            account_data.write(&output_path).with_context(|| {
                format!("failed to write account data to file: {}", output_path.display())
            })?;

            println!("Faucet account file successfully created at: {}", output_path.display());
        },

        Command::CreateApiKeys { count: key_count } => {
            let mut rng = ChaCha20Rng::from_seed(rand::random());
            let keys = (0..key_count)
                .map(|_| ApiKey::generate(&mut rng).encode())
                .collect::<Vec<_>>()
                .join(",");
            println!("{keys}");
        },
    }

    Ok(())
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
    use std::env::temp_dir;
    use std::num::NonZeroUsize;
    use std::process::Stdio;
    use std::str::FromStr;
    use std::time::{Duration, Instant};

    use fantoccini::ClientBuilder;
    use miden_node_utils::grpc::UrlExt;
    use serde_json::{Map, json};
    use tokio::io::AsyncBufReadExt;
    use tokio::time::sleep;
    use url::Url;

    use crate::stub_rpc_api::serve_stub;
    use crate::{Cli, FaucetNetwork, run_faucet_command};

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

        let faucet_account_path = temp_dir().join("faucet.mac");

        // Create faucet account
        run_faucet_command(Cli {
            command: crate::Command::CreateFaucetAccount {
                output: faucet_account_path.clone(),
                token_symbol: "TEST".to_string(),
                decimals: 6,
                max_supply: 1_000_000_000_000,
            },
        })
        .await
        .unwrap();

        // Start the faucet connected to the stub
        // Use std::thread to launch faucet - avoids Send requirements
        let endpoint_clone = Url::parse("http://localhost:8080").unwrap();
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
                        endpoint: endpoint_clone,
                        network: FaucetNetwork::Testnet,
                        node_url: stub_node_url,
                        timeout: Duration::from_millis(5000),
                        asset_amounts: vec![100, 500, 1000],
                        api_keys: vec![],
                        pow_secret: None,
                        pow_challenge_lifetime: Duration::from_secs(30),
                        pow_cleanup_interval: Duration::from_secs(1),
                        pow_growth_rate: NonZeroUsize::new(1).unwrap(),
                        pow_baseline: 12,
                        faucet_account_path: faucet_account_path.clone(),
                        remote_tx_prover_url: None,
                        open_telemetry: false,
                    },
                })
                .await
                .unwrap();
            });
        });

        // Wait for faucet to be up
        let endpoint = Url::parse("http://localhost:8080").unwrap();
        let addr = endpoint.to_socket().unwrap();
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

        endpoint
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
}
