use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use miden_node_block_producer::{DEFAULT_VALIDATOR_TIMEOUT, Sequencer};
use miden_node_proto::clients::{
    Builder,
    NtxBuilderClient,
    RemoteProverClient,
    RpcClient,
    SequencerClient,
    ValidatorClient,
    WantsConnection,
};
use miden_node_rpc::{Rpc, RpcMode, SequencerInternal};
use miden_node_store::State;
use miden_node_utils::clap::{GrpcOptionsInternal, duration_to_human_readable_string};
use miden_node_utils::formatting::format_endpoint;
use miden_node_utils::shutdown::CancellationToken;
use miden_node_utils::tasks::Tasks;
use tokio::net::TcpListener;
use url::Url;

use super::block_producer::BlockProducerOptions;
use super::rpc::SyncOptions;
use super::runtime::{RuntimeConfig, RuntimeOptions};
use super::store::StoreOptions;

// RUNTIME MODES
// ================================================================================================

#[derive(clap::Args, Clone, Debug)]
pub struct SequencerCommand {
    #[command(flatten)]
    pub runtime: RuntimeOptions,

    #[command(flatten)]
    pub external_services: SequencerExternalServiceOptions,

    #[command(flatten)]
    pub block_producer: BlockProducerOptions,

    #[command(flatten)]
    pub store: StoreOptions,

    /// Socket address at which to serve the internal sequencer API.
    #[arg(
        long = "internal.listen",
        env = "MIDEN_NODE_SEQUENCER_INTERNAL_LISTEN",
        value_name = "LISTEN"
    )]
    pub internal: Option<SocketAddr>,
}

impl SequencerCommand {
    pub async fn handle(self, shutdown: CancellationToken) -> anyhow::Result<()> {
        self.log_starting();
        let runtime = self.runtime.runtime_config(&self.store);
        self.block_producer.validate()?;
        let network_tx_auth = self.runtime.rpc.network_tx_auth()?;
        let (validator_client, validator_monitor) =
            self.external_services.validator_client_and_monitor()?;
        let (ntx_builder_client, ntx_builder_monitor) =
            self.external_services.ntx_builder_client_and_monitor()?;
        let batch_prover_monitor =
            remote_prover_monitor(self.block_producer.batch.prover_url.as_ref())?;
        let block_prover_monitor =
            remote_prover_monitor(self.block_producer.block_prover.url.as_ref())?;
        let state = load_state(&runtime).await?;
        let _disk_monitor = state.spawn_disk_monitor(shutdown.clone());

        let sequencer = Sequencer {
            store: Arc::clone(&state),
            validator_url: self.external_services.validator_url.clone(),
            validator_timeout: self.external_services.validator_timeout,
            batch_prover_url: self.block_producer.batch.prover_url,
            block_prover_url: self.block_producer.block_prover.url,
            batch_interval: self.block_producer.batch.interval,
            block_interval: self.block_producer.block.interval,
            max_txs_per_batch: self.block_producer.batch.max_txs,
            max_batches_per_block: self.block_producer.block.max_batches,
            max_concurrent_proofs: self.block_producer.block.max_concurrent_proofs,
            mempool_tx_capacity: self.block_producer.mempool.tx_capacity,
            batch_workers: self.block_producer.batch.workers,
        }
        .spawn(shutdown.clone())
        .await
        .context("failed to spawn sequencer")?;
        let block_producer = sequencer.api();

        let rpc = Rpc {
            listener: bind_rpc(runtime.rpc_listen).await?,
            store: state,
            mode: RpcMode::sequencer(block_producer.clone(), validator_client),
            ntx_builder: Some(ntx_builder_client),
            grpc_options: runtime.external_grpc_options,
            network_tx_auth,
        };
        let mut tasks = Tasks::new();
        tasks.spawn("sequencer", sequencer.wait());
        tasks.spawn("RPC server", rpc.serve(shutdown.clone()));
        tasks.spawn_infallible(
            "validator connection monitor",
            validator_monitor.monitor::<ValidatorClient>("validator", shutdown.clone()),
        );
        tasks.spawn_infallible(
            "ntx-builder connection monitor",
            ntx_builder_monitor.monitor::<NtxBuilderClient>("ntx-builder", shutdown.clone()),
        );
        if let Some(batch_prover_monitor) = batch_prover_monitor {
            tasks.spawn_infallible(
                "batch prover connection monitor",
                batch_prover_monitor
                    .monitor::<RemoteProverClient>("batch-prover", shutdown.clone()),
            );
        }
        if let Some(block_prover_monitor) = block_prover_monitor {
            tasks.spawn_infallible(
                "block prover connection monitor",
                block_prover_monitor
                    .monitor::<RemoteProverClient>("block-prover", shutdown.clone()),
            );
        }
        if let Some(internal_listen) = self.internal {
            let sequencer_internal = SequencerInternal {
                listener: bind_rpc(internal_listen).await?,
                block_producer,
                grpc_options: GrpcOptionsInternal::from(runtime.external_grpc_options),
            };
            tasks.spawn("sequencer internal server", sequencer_internal.serve(shutdown.clone()));
        }

        tasks.join_next_or_cancelled(shutdown).await
    }

    fn log_starting(&self) {
        tracing::info!(
            target: crate::LOG_TARGET,
            {
                service.name = "miden-node",
                service.version = env!("CARGO_PKG_VERSION"),
                node.role = "sequencer",
                rpc.listen = %self.runtime.rpc.listen,
                internal.listen = %self.internal.map_or_else(
                    || "disabled".to_owned(),
                    |address| address.to_string(),
                ),
                data.directory = %self.runtime.data_directory.display(),
                validator.endpoint = %format_endpoint(&self.external_services.validator_url),
                ntx_builder.endpoint = %format_endpoint(&self.external_services.ntx_builder_url),
                block.interval = %humantime::Duration::from(self.block_producer.block.interval),
                batch.interval = %humantime::Duration::from(self.block_producer.batch.interval),
                store.sqlite.connection_pool_size = self.store.sqlite.connection_pool_size.get(),
            },
            "Starting node",
        );
    }
}

#[derive(clap::Args, Clone, Debug)]
pub struct SequencerExternalServiceOptions {
    /// The validator service gRPC URL.
    #[arg(long = "validator.url", env = "MIDEN_NODE_VALIDATOR_URL", value_name = "URL")]
    pub validator_url: Url,

    /// Request timeout for calls to the validator service.
    ///
    /// Bounds the sequencer's `sign_block` call so a dropped validator connection fails fast and
    /// retries, rather than stalling block production until the OS-level TCP timeout.
    #[arg(
        long = "validator.timeout",
        env = "MIDEN_NODE_VALIDATOR_TIMEOUT",
        default_value = duration_to_human_readable_string(DEFAULT_VALIDATOR_TIMEOUT),
        value_parser = humantime::parse_duration,
        value_name = "DURATION"
    )]
    pub validator_timeout: Duration,

    /// The network transaction builder service gRPC URL.
    #[arg(long = "ntx-builder.url", env = "MIDEN_NODE_NTX_BUILDER_URL", value_name = "URL")]
    pub ntx_builder_url: Url,
}

impl SequencerExternalServiceOptions {
    fn validator_client_and_monitor(
        &self,
    ) -> anyhow::Result<(ValidatorClient, Builder<WantsConnection>)> {
        let builder = Builder::new(self.validator_url.clone())
            .with_tls()?
            .with_timeout(self.validator_timeout)
            .without_metadata_version()
            .without_metadata_genesis()
            .with_otel_context_injection();
        let client = builder.clone().connect_lazy::<ValidatorClient>();
        Ok((client, builder))
    }

    fn ntx_builder_client_and_monitor(
        &self,
    ) -> anyhow::Result<(NtxBuilderClient, Builder<WantsConnection>)> {
        let builder = Builder::new(self.ntx_builder_url.clone())
            .with_tls()?
            .without_timeout()
            .without_metadata_version()
            .without_metadata_genesis()
            .with_otel_context_injection();
        let client = builder.clone().connect_lazy::<NtxBuilderClient>();
        Ok((client, builder))
    }
}

#[derive(clap::Args, Clone, Debug)]
pub struct FullNodeCommand {
    #[command(flatten)]
    pub runtime: RuntimeOptions,

    #[command(flatten)]
    pub sync: SyncOptions,

    #[command(flatten)]
    pub store: StoreOptions,

    /// The validator service gRPC URL.
    #[arg(
        long = "validator.url",
        env = "MIDEN_NODE_VALIDATOR_URL",
        value_name = "URL",
        requires = "sequencer_url"
    )]
    pub validator_url: Option<Url>,

    /// The sequencer's internal service gRPC URL.
    #[arg(
        long = "sequencer.internal.url",
        env = "MIDEN_NODE_SEQUENCER_INTERNAL_URL",
        value_name = "URL",
        requires = "validator_url"
    )]
    pub sequencer_url: Option<Url>,
}

impl FullNodeCommand {
    pub async fn handle(self, shutdown: CancellationToken) -> anyhow::Result<()> {
        self.log_starting();
        let runtime = self.runtime.runtime_config(&self.store);
        let source_rpc = self.sync.source_rpc_client()?;
        let (validator_client, validator_monitor) = self
            .validator_client_and_monitor()
            .map_or((None, None), |(client, monitor)| (Some(client), Some(monitor)));
        let (sequencer_client, sequencer_monitor) = self
            .sequencer_client_and_monitor()
            .map_or((None, None), |(client, monitor)| (Some(client), Some(monitor)));
        let network_tx_auth = self.runtime.rpc.network_tx_auth()?;
        let state = load_state(&runtime).await?;
        let _disk_monitor = state.spawn_disk_monitor(shutdown.clone());

        let rpc = Rpc {
            listener: bind_rpc(runtime.rpc_listen).await?,
            store: state,
            mode: RpcMode::full_node(
                source_rpc,
                self.sync.readiness_threshold,
                validator_client,
                sequencer_client,
            ),
            ntx_builder: None,
            grpc_options: runtime.external_grpc_options,
            network_tx_auth,
        };
        let mut tasks = Tasks::new();
        tasks.spawn("RPC server", rpc.serve(shutdown.clone()));
        if let Some(validator_monitor) = validator_monitor {
            tasks.spawn_infallible(
                "validator connection monitor",
                validator_monitor.monitor::<ValidatorClient>("validator", shutdown.clone()),
            );
        }
        if let Some(sequencer_monitor) = sequencer_monitor {
            tasks.spawn_infallible(
                "sequencer connection monitor",
                sequencer_monitor.monitor::<SequencerClient>("sequencer", shutdown.clone()),
            );
        }

        tasks.join_next_or_cancelled(shutdown).await
    }

    fn log_starting(&self) {
        tracing::info!(
            target: crate::LOG_TARGET,
            {
                service.name = "miden-node",
                service.version = env!("CARGO_PKG_VERSION"),
                node.role = "full",
                rpc.listen = %self.runtime.rpc.listen,
                data.directory = %self.runtime.data_directory.display(),
                sync.block_source.endpoint = %format_endpoint(&self.sync.block_source_url),
                sync.ready_threshold = self.sync.readiness_threshold,
                validator.endpoint = %self.validator_url.as_ref().map_or_else(
                    || "disabled".to_owned(),
                    format_endpoint,
                ),
                sequencer.endpoint = %self.sequencer_url.as_ref().map_or_else(
                    || "disabled".to_owned(),
                    format_endpoint,
                ),
                store.sqlite.connection_pool_size = self.store.sqlite.connection_pool_size.get(),
            },
            "Starting node",
        );
    }

    fn sequencer_client_and_monitor(&self) -> Option<(SequencerClient, Builder<WantsConnection>)> {
        self.sequencer_url.as_ref().map(|url| {
            let builder = Builder::new(url.clone())
                .with_tls()
                .expect("TLS is enabled")
                .with_timeout(Duration::from_secs(5))
                .without_metadata_version()
                .without_metadata_genesis()
                .with_otel_context_injection();
            let client = builder.clone().connect_lazy::<SequencerClient>();
            (client, builder)
        })
    }

    fn validator_client_and_monitor(&self) -> Option<(ValidatorClient, Builder<WantsConnection>)> {
        self.validator_url.as_ref().map(|url| {
            let builder = Builder::new(url.clone())
                .with_tls()
                .expect("TLS is enabled")
                .with_timeout(Duration::from_secs(5))
                .without_metadata_version()
                .without_metadata_genesis()
                .with_otel_context_injection();
            let client = builder.clone().connect_lazy::<ValidatorClient>();
            (client, builder)
        })
    }
}

impl SyncOptions {
    fn source_rpc_client(&self) -> anyhow::Result<RpcClient> {
        Ok(Builder::new(self.block_source_url.clone())
            .with_tls()?
            .without_timeout()
            .without_metadata_version()
            .without_metadata_genesis()
            .with_otel_context_injection()
            .connect_lazy::<RpcClient>())
    }
}

async fn load_state(runtime: &RuntimeConfig) -> anyhow::Result<Arc<State>> {
    let state = State::load_with_database_options(
        &runtime.data_directory,
        runtime.storage_options.clone(),
        runtime.database_options,
    )
    .await
    .context("failed to load state")?;

    Ok(Arc::new(state))
}

async fn bind_rpc(listen: SocketAddr) -> anyhow::Result<TcpListener> {
    TcpListener::bind(listen)
        .await
        .with_context(|| format!("failed to bind RPC listener to {listen}"))
}

fn remote_prover_monitor(
    endpoint: Option<&Url>,
) -> anyhow::Result<Option<Builder<WantsConnection>>> {
    endpoint
        .map(|endpoint| {
            Ok(Builder::new(endpoint.clone())
                .with_tls()?
                .without_timeout()
                .without_metadata_version()
                .without_metadata_genesis()
                .without_auth_header()
                .with_otel_context_injection())
        })
        .transpose()
}
