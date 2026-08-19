//! Public module for share clap pieces to reduce duplication

use std::time::Duration;

#[cfg(feature = "rocksdb")]
mod rocksdb;
#[cfg(feature = "rocksdb")]
pub use rocksdb::*;

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const TEST_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

// Formats a Duration into a human-readable string for display in clap help text and yields a
// &'static str by _leaking_ the string deliberately.
pub fn duration_to_human_readable_string(duration: Duration) -> &'static str {
    Box::new(humantime::format_duration(duration).to_string()).leak()
}

#[derive(clap::Args, Copy, Clone, Debug, PartialEq, Eq)]
pub struct GrpcOptions {
    /// Maximum duration a gRPC request is allocated before being dropped by the server.
    ///
    /// This may occur if the server is overloaded or due to an internal bug.
    #[arg(
        long = "grpc.timeout",
        default_value = duration_to_human_readable_string(DEFAULT_REQUEST_TIMEOUT),
        value_parser = humantime::parse_duration,
        value_name = "DURATION"
    )]
    pub request_timeout: Duration,
}

impl Default for GrpcOptions {
    fn default() -> Self {
        Self { request_timeout: DEFAULT_REQUEST_TIMEOUT }
    }
}

impl GrpcOptions {
    pub fn test() -> Self {
        Self { request_timeout: TEST_REQUEST_TIMEOUT }
    }
}

/// Collection of per usage storage backend configurations, plus store write-path tuning.
#[derive(clap::Args, Clone, Debug, PartialEq, Eq)]
pub struct StorageOptions {
    #[cfg(feature = "rocksdb")]
    #[clap(flatten)]
    pub account_tree: AccountTreeRocksDbOptions,
    #[cfg(feature = "rocksdb")]
    #[clap(flatten)]
    pub nullifier_tree: NullifierTreeRocksDbOptions,
    #[cfg(feature = "rocksdb")]
    #[clap(flatten)]
    pub account_state_forest: AccountStateForestRocksDbOptions,

    /// Whether the store's apply-block thread pool runs at raised OS thread priority (best-effort).
    #[arg(
        id = "apply_block_thread_priority",
        long = "apply_block.thread_priority",
        default_value_t = true,
        action = clap::ArgAction::Set,
        value_name = "BOOL"
    )]
    pub apply_block_thread_priority: bool,
}

impl Default for StorageOptions {
    fn default() -> Self {
        Self {
            #[cfg(feature = "rocksdb")]
            account_tree: AccountTreeRocksDbOptions::default(),
            #[cfg(feature = "rocksdb")]
            nullifier_tree: NullifierTreeRocksDbOptions::default(),
            #[cfg(feature = "rocksdb")]
            account_state_forest: AccountStateForestRocksDbOptions::default(),
            apply_block_thread_priority: true,
        }
    }
}

impl StorageOptions {
    /// Benchmark setup.
    ///
    /// These values were determined during development of `LargeSmt`
    pub fn bench() -> Self {
        #[cfg(feature = "rocksdb")]
        {
            let account_tree = AccountTreeRocksDbOptions {
                max_open_fds: self::rocksdb::BENCH_ROCKSDB_MAX_OPEN_FDS,
                cache_size_in_bytes: self::rocksdb::DEFAULT_ROCKSDB_CACHE_SIZE,
                durability_mode: None,
            };
            let nullifier_tree = NullifierTreeRocksDbOptions {
                max_open_fds: BENCH_ROCKSDB_MAX_OPEN_FDS,
                cache_size_in_bytes: DEFAULT_ROCKSDB_CACHE_SIZE,
                durability_mode: None,
            };
            let account_state_forest = AccountStateForestRocksDbOptions {
                max_open_fds: BENCH_ROCKSDB_MAX_OPEN_FDS,
                cache_size_in_bytes: DEFAULT_ROCKSDB_CACHE_SIZE,
                durability_mode: None,
            };
            Self {
                account_tree,
                nullifier_tree,
                account_state_forest,
                apply_block_thread_priority: true,
            }
        }
        #[cfg(not(feature = "rocksdb"))]
        Self::default()
    }
}
