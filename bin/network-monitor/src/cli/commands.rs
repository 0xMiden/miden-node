//! CLI command definitions.

use clap::{Parser, Subcommand};
use url::Url;

use crate::commands::{deploy_accounts, start_monitor};
use crate::config::MonitorConfig;

/// Main CLI structure.
#[derive(Parser)]
#[command(version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

/// Available commands.
#[derive(Subcommand)]
pub enum Command {
    /// Starts the network monitor
    Start(MonitorConfig),

    /// Deploys a new account to the network.
    DeployAccount(DeployAccountConfig),
}

/// Configuration for the deploy account command.
#[derive(Parser)]
pub struct DeployAccountConfig {
    /// The URL of the RPC service.
    #[arg(
        long = "rpc-url",
        env = "MIDEN_MONITOR_RPC_URL",
        help = "The URL of the RPC service"
    )]
    pub rpc_url: Url,

    /// Path for the wallet account file.
    #[arg(
        long = "wallet-file",
        env = "MIDEN_MONITOR_WALLET_FILE",
        default_value = "wallet_account.bin",
        help = "Path where the wallet account will be saved"
    )]
    pub wallet_file: String,

    /// Path for the counter program account file.
    #[arg(
        long = "counter-file",
        env = "MIDEN_MONITOR_COUNTER_FILE",
        default_value = "counter_program.bin",
        help = "Path where the counter program account will be saved"
    )]
    pub counter_file: String,
}

impl Cli {
    /// Execute the parsed command.
    pub async fn execute(self) -> anyhow::Result<()> {
        match self.command {
            Command::Start(config) => start_monitor(config).await,
            Command::DeployAccount(config) => deploy_accounts(config).await,
        }
    }
}
