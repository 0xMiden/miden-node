use miden_node_utils::logging::{OpenTelemetry, setup_tracing};
use miden_node_utils::panic;
use miden_remote_prover::COMPONENT;
use tracing::info;

use crate::commands::Cli;

pub(crate) mod commands;
pub(crate) mod proxy;
pub(crate) mod utils;

#[panic::main]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use clap::Parser;

    setup_tracing(OpenTelemetry::Enabled)?;
    info!(target: COMPONENT, "Tracing initialized");

    // read command-line args
    let cli = Cli::parse();

    // execute cli action
    cli.execute().await
}
