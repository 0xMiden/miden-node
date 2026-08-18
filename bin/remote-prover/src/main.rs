use anyhow::Context;
use clap::Parser;

mod server;

const COMPONENT: &str = "miden-prover";
const LOG_TARGET: &str = "user::miden-prover";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let server = server::Server::parse();

    let _otel_guard = miden_node_utils::logging::setup_tracing(server.open_telemetry())?;

    miden_node_utils::shutdown::run_with_shutdown("miden-remote-prover", |shutdown| async move {
        let (handle, _port) = server.spawn(shutdown).await.context("failed to spawn server")?;

        handle.await.context("proof server panicked").flatten()
    })
    .await
}
