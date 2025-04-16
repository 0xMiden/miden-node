#![recursion_limit = "256"]
use std::time::Duration;

use anyhow::Result;
use miden_node_builder::NodeBuilder;

#[tokio::main]
async fn main() -> Result<()> {
    // Create a temporary directory for the node's data
    let data_dir = tempfile::tempdir()?.into_path();

    // Create a node builder with default settings
    let node = NodeBuilder::new(data_dir)
        .with_block_interval(Duration::from_millis(1000))
        .with_batch_interval(Duration::from_millis(1000))
        .with_telemetry(true);

    // Start the node
    let handle = node.start().await?;
    println!("Node started at {}", handle.rpc_url());

    // Wait for 10 seconds
    tokio::time::sleep(Duration::from_secs(10)).await;

    // Stop the node
    handle.stop().await?;
    println!("Node stopped");

    Ok(())
}
