use std::{
    fmt::{Display, Formatter},
    path::PathBuf,
};

use miden_node_utils::config::{DEFAULT_FAUCET_SERVER_PORT, DEFAULT_NODE_RPC_PORT};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::types::AssetOptions;

// Faucet config
// ================================================================================================

/// Default path to the faucet account file
pub const DEFAULT_FAUCET_ACCOUNT_PATH: &str = "accounts/faucet.mac";

/// Default timeout for RPC requests
pub const DEFAULT_RPC_TIMEOUT_MS: u64 = 10000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaucetConfig {
    /// The port at which to serve the faucet's REST API.
    pub endpoint: Url,
    /// Node RPC gRPC endpoint in the format `http://<host>[:<port>]`
    pub node_url: Url,
    /// Timeout for RPC requests in milliseconds
    pub timeout_ms: u64,
    /// Possible options on the amount of asset that should be dispersed on each faucet request
    pub asset_amount_options: AssetOptions,
    /// Path to the faucet account file
    pub faucet_account_path: PathBuf,
}

impl Display for FaucetConfig {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "{{ endpoint: \"{}\", node_url: \"{}\", timeout_ms: \"{}\", asset_amount_options: {:?}, faucet_account_path: \"{}\" }}",
            self.endpoint, self.node_url, self.timeout_ms, self.asset_amount_options, self.faucet_account_path.display()
        ))
    }
}

impl Default for FaucetConfig {
    fn default() -> Self {
        Self {
            endpoint: Url::parse(format!("http://0.0.0.0:{DEFAULT_FAUCET_SERVER_PORT}").as_str())
                .unwrap(),
            node_url: Url::parse(format!("http://127.0.0.1:{DEFAULT_NODE_RPC_PORT}").as_str())
                .unwrap(),
            timeout_ms: DEFAULT_RPC_TIMEOUT_MS,
            // SAFETY: These amounts are all less than the maximum.
            asset_amount_options: AssetOptions::new(vec![100, 500, 1_000]).unwrap(),
            faucet_account_path: DEFAULT_FAUCET_ACCOUNT_PATH.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::net::TcpListener;

    use super::FaucetConfig;

    #[tokio::test]
    async fn default_faucet_config() {
        // Default does not panic
        let config = FaucetConfig::default();
        // Default can bind
        let socket_addrs = config.endpoint.socket_addrs(|| None).unwrap();
        let socket_addr = socket_addrs.into_iter().next().unwrap();
        let _listener = TcpListener::bind(socket_addr).await.unwrap();
    }
}
