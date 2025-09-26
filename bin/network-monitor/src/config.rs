//! Network monitor configuration.
//!
//! This module contains the configuration structures and constants for the network monitor.

use clap::Parser;
use url::Url;

// MONITOR CONFIGURATION CONSTANTS
// ================================================================================================

const DEFAULT_RPC_URL: &str = "http://localhost:50051";
const DEFAULT_REMOTE_PROVER_URLS: &str = "http://localhost:50052";
const DEFAULT_FAUCET_URL: &str = "http://localhost:8080";
const DEFAULT_PORT: u16 = 3000;

/// Configuration for the monitor.
///
/// This struct contains the configuration for the monitor.
#[derive(Debug, Clone, Parser)]
#[command(name = "miden-network-monitor")]
#[command(about = "A network monitor for Miden node services")]
#[command(version)]
pub struct MonitorConfig {
    /// The URL of the RPC service.
    #[arg(
        long = "rpc-url",
        env = "MIDEN_MONITOR_RPC_URL",
        default_value = DEFAULT_RPC_URL,
        help = "The URL of the RPC service"
    )]
    pub rpc_url: Url,

    /// The URLs of the remote provers for status checking (comma-separated).
    #[arg(
        long = "remote-prover-urls",
        env = "MIDEN_MONITOR_REMOTE_PROVER_URLS",
        default_value = DEFAULT_REMOTE_PROVER_URLS,
        value_delimiter = ',',
        help = "The URLs of the remote provers for status checking (comma-separated)"
    )]
    pub remote_prover_urls: Vec<Url>,

    /// The URL of the faucet service for testing (optional).
    #[arg(
        long = "faucet-url",
        env = "MIDEN_MONITOR_FAUCET_URL",
        default_value = DEFAULT_FAUCET_URL,
        help = "The URL of the faucet service for testing (optional)"
    )]
    pub faucet_url: Option<Url>,

    /// The port of the monitor.
    #[arg(
        long = "port",
        short = 'p',
        env = "MIDEN_MONITOR_PORT",
        default_value_t = DEFAULT_PORT,
        help = "The port of the monitor"
    )]
    pub port: u16,

    /// Whether to enable OpenTelemetry.
    #[arg(
        long = "enable-otel",
        env = "MIDEN_MONITOR_ENABLE_OTEL",
        action = clap::ArgAction::SetTrue,
        help = "Whether to enable OpenTelemetry"
    )]
    pub enable_otel: bool,
}
