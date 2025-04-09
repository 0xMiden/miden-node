use std::path::PathBuf;

use clap::{Parser, Subcommand};
use seeding::seed_store;
use store_tests::{bench_check_nullifiers_by_prefix, bench_sync_notes, bench_sync_state};

mod endpoints_metrics;
mod seeding;
mod seeding_metrics;
mod store_tests;

#[derive(Parser)]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Create and store blocks into the store. Create a given number of accounts, where each
    /// account consumes a note created from a faucet.
    SeedStore {
        /// Directory in which to store the database and raw block data. If the directory contains
        /// a database dump file, it will be replaced.
        #[arg(short, long, value_name = "DATA_DIRECTORY")]
        data_directory: PathBuf,

        /// Number of accounts to create.
        #[arg(short, long, value_name = "NUM_ACCOUNTS")]
        num_accounts: usize,

        /// Percentage of accounts that will be created as public accounts. The rest will be
        /// private accounts.
        #[arg(short, long, value_name = "PUBLIC_ACCOUNTS_PERCENTAGE", default_value = "0")]
        public_accounts_percentage: u8,
    },

    /// Benchmark the performance of the store endpoints.
    BenchmarkStore {
        /// Store endpoint to test against.
        #[arg(value_name = "ENDPOINT")]
        endpoint: Endpoint,

        /// Directory that contains the database dump file.
        #[arg(short, long, value_name = "DATA_DIRECTORY")]
        data_directory: PathBuf,

        /// Iterations of the sync request.
        #[arg(short, long, value_name = "ITERATIONS")]
        iterations: usize,

        /// Concurrency level of the sync request. Represents the number of request that
        /// can be sent in parallel.
        #[arg(short, long, value_name = "CONCURRENCY", default_value = "1")]
        concurrency: usize,
    },
}

/// Store endpoint.
#[derive(clap::ValueEnum, Clone, Copy)]
pub enum Endpoint {
    CheckNullifiersByPrefix,
    SyncState,
    SyncNotes,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::SeedStore {
            data_directory,
            num_accounts,
            public_accounts_percentage,
        } => {
            seed_store(data_directory, num_accounts, public_accounts_percentage).await;
        },
        Command::BenchmarkStore {
            endpoint,
            data_directory,
            iterations,
            concurrency,
        } => match endpoint {
            Endpoint::CheckNullifiersByPrefix => {
                bench_check_nullifiers_by_prefix(data_directory, iterations, concurrency, 10).await;
            },
            Endpoint::SyncState => {
                bench_sync_state(data_directory, iterations, concurrency).await;
            },
            Endpoint::SyncNotes => {
                bench_sync_notes(data_directory, iterations, concurrency).await;
            },
        },
    }
}
