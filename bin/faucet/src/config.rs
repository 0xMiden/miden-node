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

/// Default rate limiter settings.
/// Limits chosen somewhat arbitrarily, but we have five assets required to load the webpage.
/// So allowing 8 in a burst seems okay.
pub const DEFAULT_IP_RATE_LIMIT_BURST_SIZE: u32 = 8;
pub const DEFAULT_IP_RATE_LIMIT_PER_SECOND: u64 = 1;
pub const DEFAULT_ACCOUNT_RATE_LIMIT_BURST_SIZE: u32 = 1;
pub const DEFAULT_ACCOUNT_RATE_LIMIT_PER_SECOND: u64 = 10;
pub const DEFAULT_API_KEY_RATE_LIMIT_BURST_SIZE: u32 = 3;
pub const DEFAULT_API_KEY_RATE_LIMIT_PER_SECOND: u64 = 10;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaucetConfig {
    /// Endpoint of the faucet in the format `<ip>:<port>`
    pub endpoint: Url,
    /// Node RPC gRPC endpoint in the format `http://<host>[:<port>]`
    pub node_url: Url,
    /// Timeout for RPC requests in milliseconds
    pub timeout_ms: u64,
    /// Possible options on the amount of asset that should be dispersed on each faucet request
    pub asset_amount_options: AssetOptions,
    /// Path to the faucet account file
    pub faucet_account_path: PathBuf,
    /// Optional: Endpoint of the remote transaction prover in the format
    /// `<protocol>://<host>[:<port>]`
    pub remote_tx_prover_url: Option<Url>,
    /// The salt to be used by the server to generate the `PoW` seed
    pub pow_salt: String,
    /// List of API keys
    pub api_keys: Vec<String>,
    /// Rate limiter configuration
    pub rate_limiter: RateLimiterConfig,
}

/// Configuration for rate limiting requests to the faucet.
///
/// We have three rate limiters:
///
/// 1. IP-based rate limiting: restricts requests from the same IP address
/// 2. Account-based rate limiting: restricts requests to `get_tokens` with the same account
/// 3. API key-based rate limiting: restricts requests using the same API key
///
/// Each rate limiter uses a token bucket algorithm with two parameters:
/// - `burst_size`: Maximum number of requests allowed in a burst before replenishment
/// - `per_second`: Rate at which tokens are replenished (1 token per X seconds)
///
/// # Examples
///
/// With the default configuration:
/// ```rust
/// let config = RateLimiterConfig {
///     ip_rate_limit_burst_size: 8,
///     ip_rate_limit_per_second: 1,
///     account_rate_limit_burst_size: 1,
///     account_rate_limit_per_second: 10,
///     api_key_rate_limit_burst_size: 3,
///     api_key_rate_limit_per_second: 10,
/// };
/// ```
///
/// This means:
/// - An IP can make up to 8 requests in quick succession, then must wait 1 second between requests
/// - An account can make 1 request, then must wait 0.1 seconds (1/10 second) before the next
///   request
/// - An API key can make up to 3 requests in quick succession, then must wait 0.1 seconds between
///   requests
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimiterConfig {
    pub ip_rate_limit_burst_size: u32,
    pub ip_rate_limit_per_second: u64,
    pub account_rate_limit_burst_size: u32,
    pub account_rate_limit_per_second: u64,
    pub api_key_rate_limit_burst_size: u32,
    pub api_key_rate_limit_per_second: u64,
}

impl Display for FaucetConfig {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "{{ endpoint: \"{}\", node_url: \"{}\", timeout_ms: \"{}\", asset_amount_options: {:?}, faucet_account_path: \"{}\", remote_tx_prover_url: \"{:?}\", pow_salt: \"{}\", api_keys: {:?} }}",
            self.endpoint, self.node_url, self.timeout_ms, self.asset_amount_options, self.faucet_account_path.display(), self.remote_tx_prover_url, self.pow_salt, self.api_keys
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
            remote_tx_prover_url: None,
            pow_salt: rand::random::<[u8; 32]>().into_iter().map(|b| b as char).collect(),
            api_keys: Vec::new(),
            rate_limiter: RateLimiterConfig {
                ip_rate_limit_burst_size: DEFAULT_IP_RATE_LIMIT_BURST_SIZE,
                ip_rate_limit_per_second: DEFAULT_IP_RATE_LIMIT_PER_SECOND,
                account_rate_limit_burst_size: DEFAULT_ACCOUNT_RATE_LIMIT_BURST_SIZE,
                account_rate_limit_per_second: DEFAULT_ACCOUNT_RATE_LIMIT_PER_SECOND,
                api_key_rate_limit_burst_size: DEFAULT_API_KEY_RATE_LIMIT_BURST_SIZE,
                api_key_rate_limit_per_second: DEFAULT_API_KEY_RATE_LIMIT_PER_SECOND,
            },
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
