use std::time::Duration;

pub mod block_producer;
pub mod bundled;
pub mod rpc;
pub mod store;

const ENV_BLOCK_PRODUCER_URL: &str = "MIDEN_NODE_BLOCK_PRODUCER_URL";
const ENV_NTX_BUILDER_URL: &str = "MIDEN_NODE_NTX_BUILDER_URL";
const ENV_BATCH_PROVER_URL: &str = "MIDEN_NODE_BATCH_PROVER_URL";
const ENV_BLOCK_PROVER_URL: &str = "MIDEN_NODE_BLOCK_PROVER_URL";
const ENV_NTX_PROVER_URL: &str = "MIDEN_NODE_NTX_PROVER_URL";
const ENV_RPC_URL: &str = "MIDEN_NODE_RPC_URL";
const ENV_STORE_URL: &str = "MIDEN_NODE_STORE_URL";
const ENV_DATA_DIRECTORY: &str = "MIDEN_NODE_DATA_DIRECTORY";
const ENV_ENABLE_OTEL: &str = "MIDEN_NODE_ENABLE_OTEL";

const DEFAULT_BLOCK_INTERVAL_MS: &str = "5000";
const DEFAULT_BATCH_INTERVAL_MS: &str = "2000";
const DEFAULT_MONITOR_INTERVAL_MS: &str = "10000";
const DEFAULT_NTX_TICKER_INTERVAL_MS: &str = "200";

fn parse_duration_ms(arg: &str) -> Result<std::time::Duration, std::num::ParseIntError> {
    arg.parse().map(Duration::from_millis)
}
