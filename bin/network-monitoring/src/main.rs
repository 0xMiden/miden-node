use std::sync::Arc;

use tokio::sync::Mutex;

mod frontend;
mod status;

use frontend::run_frontend;
use status::{MonitoringConfig, NetworkStatus, SharedStatus, check_status};

#[tokio::main]
async fn main() {
    // Load configuration from environment variables
    let config = match MonitoringConfig::from_env() {
        Ok(config) => {
            println!("Loaded configuration:");
            println!("  RPC URL: {}", config.rpc_url);
            println!(
                "  Remote Prover URLs: {}",
                config
                    .remote_prover_urls
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            println!("  Port: {}", config.port);
            config
        },
        Err(e) => {
            eprintln!("Failed to load configuration: {e}");
            std::process::exit(1);
        },
    };

    // Initialize shared status state
    let shared_status: SharedStatus =
        Arc::new(Mutex::new(NetworkStatus { services: Vec::new(), last_updated: 0 }));

    // Start a task with a frontend that displays the network status.
    let frontend_status = shared_status.clone();
    let frontend_config = config.clone();
    let frontend_task = tokio::spawn(run_frontend(frontend_status, frontend_config));

    // Start a task to hit status endpoint periodically in the different components.
    let status_status = shared_status.clone();
    let status_config = config.clone();
    let status_task = tokio::spawn(check_status(status_status, status_config));

    // Wait for either task to complete or fail, then abort the other
    let (frontend_result, status_result) = tokio::join!(frontend_task, status_task);

    // Check if either task failed and exit with error code
    match (frontend_result, status_result) {
        (Ok(_), Ok(_)) => {
            println!("Both tasks completed successfully");
        },
        (Err(e), Ok(_)) => {
            eprintln!("Frontend task failed: {e}");
            eprintln!("Status task completed normally");
            std::process::exit(1);
        },
        (Ok(_), Err(e)) => {
            eprintln!("Status task failed: {e}");
            eprintln!("Frontend task completed normally");
            std::process::exit(1);
        },
        (Err(e1), Err(e2)) => {
            eprintln!("Both tasks failed:");
            eprintln!("  Frontend task error: {e1}");
            eprintln!("  Status task error: {e2}");
            std::process::exit(1);
        },
    }

    // If any of the tasks fail, terminate the program.
}
