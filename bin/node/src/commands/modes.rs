use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use miden_node_block_producer::{DEFAULT_VALIDATOR_TIMEOUT, Sequencer};
use miden_node_proto::clients::{
    Builder,
    NtxBuilderClient,
    RpcClient,
    SequencerClient,
    ValidatorClient,
};
use miden_node_rpc::{PreAuthSubmission, Rpc, RpcMode, SequencerInternal};
use miden_node_store::State;
use miden_node_utils::clap::{GrpcOptionsInternal, duration_to_human_readable_string};
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
        let runtime = self.runtime.runtime_config(&self.store);
        self.block_producer.validate()?;
        let network_tx_auth = self.runtime.rpc.network_tx_auth()?;
        let state = load_state(&runtime).await?;
        let _disk_monitor = state.spawn_disk_monitor(shutdown.clone());

        let sequencer = Sequencer {
            store: Arc::clone(&state),
            validator_urls: self.external_services.validator_urls.clone(),
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
            mode: RpcMode::sequencer(
                block_producer.clone(),
                self.external_services.validator_clients()?,
            ),
            ntx_builder: Some(self.external_services.ntx_builder_client()?),
            grpc_options: runtime.external_grpc_options,
            network_tx_auth,
        };
        let mut tasks = Tasks::new();
        tasks.spawn("sequencer", sequencer.wait());
        tasks.spawn("RPC server", rpc.serve(shutdown.clone()));
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
}

#[derive(clap::Args, Clone, Debug)]
pub struct SequencerExternalServiceOptions {
    /// The validator service gRPC URLs.
    ///
    /// One URL per validator; repeat the argument or comma-separate the values. Transactions are
    /// submitted to, and blocks are signed by, every validator.
    #[arg(
        long = "validator.url",
        env = "MIDEN_NODE_VALIDATOR_URL",
        value_name = "URL",
        value_delimiter = ',',
        required = true
    )]
    pub validator_urls: Vec<Url>,

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
    fn validator_clients(&self) -> anyhow::Result<Vec<ValidatorClient>> {
        self.validator_urls
            .iter()
            .map(|url| {
                Ok(Builder::new(url.clone())
                    .with_tls()?
                    .with_timeout(self.validator_timeout)
                    .without_metadata_version()
                    .without_metadata_genesis()
                    .with_otel_context_injection()
                    .connect_lazy::<ValidatorClient>())
            })
            .collect()
    }

    fn ntx_builder_client(&self) -> anyhow::Result<NtxBuilderClient> {
        Ok(Builder::new(self.ntx_builder_url.clone())
            .with_tls()?
            .without_timeout()
            .without_metadata_version()
            .without_metadata_genesis()
            .with_otel_context_injection()
            .connect_lazy::<NtxBuilderClient>())
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

    /// The validator service gRPC URLs.
    ///
    /// One URL per validator; repeat the argument or comma-separate the values. Transactions are
    /// submitted to every validator.
    #[arg(
        long = "validator.url",
        env = "MIDEN_NODE_VALIDATOR_URL",
        value_name = "URL",
        value_delimiter = ',',
        requires = "sequencer_url"
    )]
    pub validator_urls: Vec<Url>,

    /// The sequencer's internal service gRPC URL.
    #[arg(
        long = "sequencer.internal.url",
        env = "MIDEN_NODE_SEQUENCER_INTERNAL_URL",
        value_name = "URL",
        requires = "validator_urls"
    )]
    pub sequencer_url: Option<Url>,
}

impl FullNodeCommand {
    pub async fn handle(self, shutdown: CancellationToken) -> anyhow::Result<()> {
        let runtime = self.runtime.runtime_config(&self.store);
        let source_rpc = self.sync.source_rpc_client()?;
        let pre_auth = self.pre_auth_submission()?;
        let network_tx_auth = self.runtime.rpc.network_tx_auth()?;
        let state = load_state(&runtime).await?;
        let _disk_monitor = state.spawn_disk_monitor(shutdown.clone());

        let rpc = Rpc {
            listener: bind_rpc(runtime.rpc_listen).await?,
            store: state,
            mode: RpcMode::full_node(source_rpc, self.sync.readiness_threshold, pre_auth),
            ntx_builder: None,
            grpc_options: runtime.external_grpc_options,
            network_tx_auth,
        };
        let mut tasks = Tasks::new();
        tasks.spawn("RPC server", rpc.serve(shutdown.clone()));

        tasks.join_next_or_cancelled(shutdown).await
    }

    fn pre_auth_submission(&self) -> anyhow::Result<Option<PreAuthSubmission>> {
        // Clap enforces that the sequencer URL and at least one validator URL come together.
        let Some(sequencer_url) = self.sequencer_url.as_ref() else {
            return Ok(None);
        };
        let sequencer = Builder::new(sequencer_url.clone())
            .with_tls()
            .expect("TLS is enabled")
            .with_timeout(Duration::from_secs(5))
            .without_metadata_version()
            .without_metadata_genesis()
            .with_otel_context_injection()
            .connect_lazy::<SequencerClient>();
        let validators = self
            .validator_urls
            .iter()
            .map(|url| {
                Builder::new(url.clone())
                    .with_tls()
                    .expect("TLS is enabled")
                    .with_timeout(Duration::from_secs(5))
                    .without_metadata_version()
                    .without_metadata_genesis()
                    .with_otel_context_injection()
                    .connect_lazy::<ValidatorClient>()
            })
            .collect();
        PreAuthSubmission::new(validators, sequencer).map(Some)
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
