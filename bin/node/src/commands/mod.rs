use std::time::Duration;

pub mod block_producer;
pub mod bundled;
pub mod rpc;
pub mod store;

const ENV_BLOCK_PRODUCER_URL: &str = "MIDEN_NODE_BLOCK_PRODUCER_URL";
const ENV_BATCH_PROVER_URL: &str = "MIDEN_NODE_BATCH_PROVER_URL";
const ENV_BLOCK_PROVER_URL: &str = "MIDEN_NODE_BLOCK_PROVER_URL";
const ENV_NTX_PROVER_URL: &str = "MIDEN_NODE_NTX_PROVER_URL";
const ENV_RPC_URL: &str = "MIDEN_NODE_RPC_URL";
const ENV_STORE_URL: &str = "MIDEN_NODE_STORE_URL";
const ENV_DATA_DIRECTORY: &str = "MIDEN_NODE_DATA_DIRECTORY";
const ENV_ENABLE_OTEL: &str = "MIDEN_NODE_ENABLE_OTEL";

const DEFAULT_BLOCK_INTERVAL: Duration = Duration::from_secs(5);
const DEFAULT_BATCH_INTERVAL: Duration = Duration::from_secs(2);
const DEFAULT_MONITOR_INTERVAL: Duration = Duration::from_secs(10);
const DEFAULT_NTX_TICKER_INTERVAL: Duration = Duration::from_millis(200);

// Formats a Duration into a human-readable string for display in clap help text.
fn duration_to_human_readable_string(duration: Duration) -> String {
    humantime::format_duration(duration).to_string()
}
