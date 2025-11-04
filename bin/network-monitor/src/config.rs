//! Network monitor configuration.
//!
//! This module contains the configuration structures and constants for the network monitor.
//! Configuration for the monitor.

use std::path::PathBuf;
use std::time::Duration;

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

    /// The interval at which to test the remote provers services.
    #[arg(
        long = "remote-prover-test-interval",
        env = "MIDEN_MONITOR_REMOTE_PROVER_TEST_INTERVAL",
        default_value = "2m",
        value_parser = humantime::parse_duration,
        help = "The interval at which to test the remote provers services"
    )]
    pub remote_prover_test_interval: Duration,

    /// The interval at which to test the faucet services.
    #[arg(
        long = "faucet-test-interval",
        env = "MIDEN_MONITOR_FAUCET_TEST_INTERVAL",
        default_value = "2m",
        value_parser = humantime::parse_duration,
        help = "The interval at which to test the faucet services"
    )]
    pub faucet_test_interval: Duration,

    /// The interval at which to check the status of the services.
    #[arg(
        long = "status-check-interval",
        env = "MIDEN_MONITOR_STATUS_CHECK_INTERVAL",
        default_value = "3s",
        value_parser = humantime::parse_duration,
        help = "The interval at which to check the status of the services"
    )]
    pub status_check_interval: Duration,

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
        default_value_t = true,
        help = "Whether to enable OpenTelemetry"
    )]
    pub enable_otel: bool,

    /// Path for the wallet account file.
    #[arg(
        long = "wallet-filepath",
        env = "MIDEN_MONITOR_WALLET_FILEPATH",
        default_value = "wallet_account.bin",
        help = "Path where the wallet account will be saved"
    )]
    pub wallet_filepath: PathBuf,

    /// Path for the counter program account file.
    #[arg(
        long = "counter-filepath",
        env = "MIDEN_MONITOR_COUNTER_FILEPATH",
        default_value = "counter_program.bin",
        help = "Path where the counter program account will be saved"
    )]
    pub counter_filepath: PathBuf,
}
