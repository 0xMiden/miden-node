use clap::Parser;
use commands::Cli;
use miden_node_utils::logging::{OpenTelemetry, setup_tracing};
use miden_remote_prover::COMPONENT;
use miden_remote_prover::server::commands;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    setup_tracing(OpenTelemetry::Enabled)?;
    info!(target: COMPONENT, "Tracing initialized");

    // read command-line args
    let cli = Cli::parse();

    // execute cli action
    cli.execute().await
}
