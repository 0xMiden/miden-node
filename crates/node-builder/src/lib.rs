use std::{collections::HashMap, path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use miden_node_block_producer::server::BlockProducer;
use miden_node_rpc::server::Rpc;
use miden_node_store::{GenesisState, Store};
use tokio::{net::TcpListener, task::JoinSet};
use url::Url;

// NODE BUILDER
// ================================================================================================

/// Builder for configuring and starting a Miden node with all components.
pub struct NodeBuilder {
    data_directory: PathBuf,
    batch_prover_url: Option<Url>,
    block_prover_url: Option<Url>,
    block_interval: Duration,
    batch_interval: Duration,
    enable_telemetry: bool,
}

impl NodeBuilder {
    // CONSTRUCTOR
    // --------------------------------------------------------------------------------------------

    /// Creates a new [`NodeBuilder`] with default settings.
    pub fn new(data_directory: PathBuf) -> Self {
        Self {
            data_directory,
            batch_prover_url: None,
            block_prover_url: None,
            block_interval: Duration::from_millis(1000),
            batch_interval: Duration::from_millis(1000),
            enable_telemetry: false,
        }
    }

    // CONFIGURATION
    // --------------------------------------------------------------------------------------------

    /// Sets the batch prover URL.
    #[must_use]
    pub fn with_batch_prover_url(mut self, url: Url) -> Self {
        self.batch_prover_url = Some(url);
        self
    }

    /// Sets the block prover URL.
    #[must_use]
    pub fn with_block_prover_url(mut self, url: Url) -> Self {
        self.block_prover_url = Some(url);
        self
    }

    /// Sets the block production interval.
    #[must_use]
    pub fn with_block_interval(mut self, interval: Duration) -> Self {
        self.block_interval = interval;
        self
    }

    /// Sets the batch production interval.
    #[must_use]
    pub fn with_batch_interval(mut self, interval: Duration) -> Self {
        self.batch_interval = interval;
        self
    }

    /// Enables or disables telemetry.
    #[must_use]
    pub fn with_telemetry(mut self, enable: bool) -> Self {
        self.enable_telemetry = enable;
        self
    }

    // START
    // --------------------------------------------------------------------------------------------

    /// Starts all node components and returns a handle to manage them.
    pub async fn start(self) -> Result<NodeHandle> {
        miden_node_utils::logging::setup_tracing(
            miden_node_utils::logging::OpenTelemetry::Disabled,
        )?;
        let mut join_set = JoinSet::new();

        // Create a store component
        let store_listener = TcpListener::bind("127.0.0.1:50051").await?;
        let store_addr = store_listener.local_addr()?;
        let genesis = GenesisState::new(vec![], 0, 0);
        Store::bootstrap(genesis, &self.data_directory).context("failed to bootstrap store")?;
        let store = Store::init(store_listener, self.data_directory)
            .await
            .context("failed to initialize store")?;
        let store_id =
            join_set.spawn(async move { store.serve().await.context("Serving store") }).id();

        // Create a block producer component
        let block_producer_listener = TcpListener::bind("127.0.0.1:50052").await?;
        let block_producer_addr = block_producer_listener.local_addr()?;
        let block_producer = BlockProducer::init(
            block_producer_listener,
            store_addr,
            self.block_prover_url,
            self.batch_prover_url,
            self.block_interval,
            self.batch_interval,
        )
        .await
        .context("failed to initialize block producer")
        .unwrap();
        let block_producer_id = join_set
            .spawn(async move { block_producer.serve().await.context("Serving block-producer") })
            .id();

        // Create an RPC component
        let rpc_listener = TcpListener::bind("127.0.0.1:50053").await?;
        let rpc = Rpc::init(rpc_listener, store_addr, block_producer_addr)
            .await
            .context("failed to initialize RPC")?;

        let rpc_id = join_set.spawn(async move { rpc.serve().await.context("Serving RPC") }).id();

        // Lookup table so we can identify the failed component.
        let component_ids = HashMap::from([
            (store_id, "store"),
            (block_producer_id, "block-producer"),
            (rpc_id, "rpc"),
        ]);

        // SAFETY: The joinset is definitely not empty.
        let component_result = join_set.join_next_with_id().await.unwrap();

        // We expect components to run indefinitely, so we treat any return as fatal.
        //
        // Map all outcomes to an error, and provide component context.
        let (id, err) = match component_result {
            Ok((id, Ok(_))) => (id, Err(anyhow::anyhow!("Component completed unexpectedly"))),
            Ok((id, Err(err))) => (id, Err(err)),
            Err(join_err) => (join_err.id(), Err(join_err).context("Joining component task")),
        };
        let component = component_ids.get(&id).unwrap_or(&"unknown");

        // We could abort and gracefully shutdown the other components, but since we're crashing the
        // node there is no point.

        err.context(format!("Component {component} failed"))
    }
}

// NODE HANDLE
// ================================================================================================

/// Handle to manage running node components.
pub struct NodeHandle {
    rpc_url: String,
    rpc_handle: tokio::task::JoinHandle<()>,
    block_producer_handle: tokio::task::JoinHandle<()>,
    store_handle: tokio::task::JoinHandle<()>,
}

impl NodeHandle {
    /// Returns the URL where the RPC server is listening.
    pub fn rpc_url(&self) -> &str {
        &self.rpc_url
    }

    /// Stops all node components.
    pub async fn stop(self) -> Result<()> {
        self.rpc_handle.abort();
        self.block_producer_handle.abort();
        self.store_handle.abort();

        // Wait for the tasks to complete
        let _ = self.rpc_handle.await;
        let _ = self.block_producer_handle.await;
        let _ = self.store_handle.await;

        Ok(())
    }
}
