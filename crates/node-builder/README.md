# Miden Node Builder

A builder crate for configuring and starting a Miden node with all its components. This crate provides a convenient way to initialize and manage a complete Miden node instance.

## Features

- Configurable node components initialization
- Support for both local and remote provers
- Configurable block and batch intervals
- Telemetry support
- Graceful component management

## Usage

```rust
use miden_node_builder::NodeBuilder;
use std::path::PathBuf;
use std::time::Duration;
use url::Url;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Create a new node builder
    let builder = NodeBuilder::new(PathBuf::from("./data"))
        // Optional: Configure batch prover URL
        .with_batch_prover_url(Url::parse("http://localhost:8080")?)
        // Optional: Configure block prover URL
        .with_block_prover_url(Url::parse("http://localhost:8081")?)
        // Optional: Configure block interval
        .with_block_interval(Duration::from_millis(1000))
        // Optional: Configure batch interval
        .with_batch_interval(Duration::from_millis(1000))
        // Optional: Enable telemetry
        .with_telemetry(true);

    // Start the node
    let node_handle = builder.start().await?;

    // Get the RPC URL
    println!("RPC server listening at: {}", node_handle.rpc_url());

    // ... do something with the node ...

    // Stop the node when done
    node_handle.stop().await?;

    Ok(())
}
```

For a complete working example, see the `simple.rs` example in the crate's source code.

## Components

The builder initializes and manages the following components:

1. **Store**: Manages the node's state and data persistence
2. **Block Producer**: Handles block production and validation
3. **RPC Server**: Provides an interface for interacting with the node

## Configuration Options

- `data_directory`: Path to store node data
- `batch_prover_url`: URL for the batch prover service (optional)
- `block_prover_url`: URL for the block prover service (optional)
- `block_interval`: Duration between block production attempts
- `batch_interval`: Duration between batch production attempts
- `enable_telemetry`: Whether to enable telemetry collection

## Error Handling

The crate uses `anyhow::Result` for error handling, providing detailed error messages when something goes wrong. All component failures are treated as fatal and will cause the node to stop.

## License
This project is [MIT licensed](../../LICENSE).
