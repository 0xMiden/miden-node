//! Network monitor configuration.
//!
//! This module contains the configuration structures and constants for the network monitor.

use url::Url;

// MONITOR CONFIGURATION CONSTANTS
// ================================================================================================

const DEFAULT_RPC_URL: &str = "http://localhost:50051";
const DEFAULT_REMOTE_PROVER_URLS: &str = "http://localhost:50052";
const DEFAULT_PORT: u16 = 3000;

const RPC_URL_ENV_VAR: &str = "MIDEN_MONITOR_RPC_URL";
const REMOTE_PROVER_URLS_ENV_VAR: &str = "MIDEN_MONITOR_REMOTE_PROVER_URLS";
const PORT_ENV_VAR: &str = "MIDEN_MONITOR_PORT";
pub const ENABLE_OTEL_ENV_VAR: &str = "MIDEN_MONITOR_ENABLE_OTEL";

/// Configuration for the monitor.
///
/// This struct contains the configuration for the monitor.
#[derive(Debug, Clone)]
pub struct MonitorConfig {
    /// The URL of the RPC service.
    pub rpc_url: Url,
    /// The URLs of the remote provers for status checking.
    pub remote_prover_urls: Vec<Url>,
    /// The port of the monitor.
    pub port: u16,
}

impl MonitorConfig {
    /// Loads the configuration from the environment variables.
    ///
    /// This function loads the configuration from the environment variables.
    /// The environment variables are:
    /// - `MIDEN_MONITOR_RPC_URL`: The URL of the RPC service.
    /// - `MIDEN_MONITOR_REMOTE_PROVER_URLS`: The URLs of the remote provers for status checking,
    ///   comma separated.
    /// - `MIDEN_MONITOR_PORT`: The port of the monitor.
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let rpc_url =
            std::env::var(RPC_URL_ENV_VAR).unwrap_or_else(|_| DEFAULT_RPC_URL.to_string());

        // Parse multiple remote prover URLs from environment variable for status checking
        let remote_prover_urls = std::env::var(REMOTE_PROVER_URLS_ENV_VAR)
            .unwrap_or_else(|_| DEFAULT_REMOTE_PROVER_URLS.to_string());

        let remote_prover_urls = remote_prover_urls
            .split(',')
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(Url::parse)
            .collect::<Result<Vec<_>, _>>()?;

        let port = std::env::var(PORT_ENV_VAR)
            .unwrap_or_else(|_| DEFAULT_PORT.to_string())
            .parse::<u16>()?;

        Ok(MonitorConfig {
            rpc_url: Url::parse(&rpc_url)?,
            remote_prover_urls,
            port,
        })
    }
}
