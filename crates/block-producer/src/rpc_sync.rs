use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use anyhow::Context;
use miden_node_proto::clients::RpcClient;
use miden_node_proto::generated::rpc::{BlockSubscriptionRequest, ProofSubscriptionRequest};
use miden_node_store::state::{BlockWriter, ProofWriter, State};
use miden_node_utils::retry::{self, Retryable};
use miden_node_utils::shutdown::CancellationToken;
use miden_node_utils::tasks::Tasks;
use miden_node_utils::tracing::miden_instrument;
use miden_protocol::block::{BlockNumber, SignedBlock};
use miden_protocol::utils::serde::Deserializable;
use tokio_stream::StreamExt;
use tonic_health::ServingStatus;
use tonic_health::server::HealthReporter;
use tracing::{info, warn};

use crate::{COMPONENT, LOG_TARGET};

pub(crate) const RECONNECT_DELAY: Duration = Duration::from_secs(5);

// RPC READINESS
// ================================================================================================

/// Tracks readiness of the RPC API service for a full-node.
///
/// Holds the gRPC [`HealthReporter`] and the readiness threshold. Created by [`Rpc::serve`]
/// once the health pair is available and passed directly into [`BlockSync`].
#[derive(Clone)]
pub struct RpcReadiness {
    reporter: HealthReporter,
    threshold: u32,
    state: Arc<AtomicU8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReadinessTransition {
    InitialNotReady,
    BecameReady,
    BecameNotReady,
    Unchanged,
}

impl RpcReadiness {
    const SERVICE_NAME: &'static str = "rpc.Api";
    const UNKNOWN: u8 = 0;
    const NOT_READY: u8 = 1;
    const READY: u8 = 2;

    pub fn new(reporter: HealthReporter, threshold: u32) -> Self {
        Self {
            reporter,
            threshold,
            state: Arc::new(AtomicU8::new(Self::UNKNOWN)),
        }
    }

    /// Updates the RPC service health status based on the upstream/local tip gap.
    pub async fn update(&self, upstream_tip: BlockNumber, local_tip: BlockNumber) {
        let gap = upstream_tip.as_u32().saturating_sub(local_tip.as_u32());
        let ready = gap <= self.threshold;
        let status = if ready {
            ServingStatus::Serving
        } else {
            ServingStatus::NotServing
        };
        self.reporter.set_service_status(Self::SERVICE_NAME, status).await;

        match self.record_transition(ready) {
            ReadinessTransition::BecameReady => {
                info!(
                    target: LOG_TARGET,
                    {
                        service.name = "miden-node",
                        service.version = env!("CARGO_PKG_VERSION"),
                        node.role = "full",
                        block.number = %local_tip,
                        sync.upstream_block = %upstream_tip,
                        sync.block_gap = gap,
                        sync.ready_threshold = self.threshold,
                    },
                    "Node ready",
                );
            },
            ReadinessTransition::BecameNotReady => {
                warn!(
                    target: LOG_TARGET,
                    {
                        block.number = %local_tip,
                        sync.upstream_block = %upstream_tip,
                        sync.block_gap = gap,
                        sync.ready_threshold = self.threshold,
                    },
                    "Node no longer ready",
                );
            },
            ReadinessTransition::InitialNotReady => {
                tracing::debug!(
                    target: LOG_TARGET,
                    {
                        block.number = %local_tip,
                        sync.upstream_block = %upstream_tip,
                        sync.block_gap = gap,
                        sync.ready_threshold = self.threshold,
                    },
                    "Node synchronizing",
                );
            },
            ReadinessTransition::Unchanged => {},
        }
    }

    fn record_transition(&self, ready: bool) -> ReadinessTransition {
        let next = if ready { Self::READY } else { Self::NOT_READY };
        match self.state.swap(next, Ordering::Relaxed) {
            previous if previous == next => ReadinessTransition::Unchanged,
            Self::UNKNOWN if ready => ReadinessTransition::BecameReady,
            Self::UNKNOWN => ReadinessTransition::InitialNotReady,
            Self::READY => ReadinessTransition::BecameNotReady,
            Self::NOT_READY => ReadinessTransition::BecameReady,
            _ => unreachable!("readiness state is one of the declared constants"),
        }
    }
}

// RPC SYNC
// ================================================================================================

/// Synchronizes local state from an upstream RPC service.
pub struct RpcSync {
    /// Read-only store state, used by both loops to read tips and subscribe to commits.
    pub state: Arc<State>,
    /// The store's block-write capability, consumed by the block sync loop.
    pub block_writer: BlockWriter,
    /// The store's proof-write capability, consumed by the proof sync loop.
    pub proof_writer: ProofWriter,
    pub source_rpc: RpcClient,
    pub readiness: RpcReadiness,
}

impl RpcSync {
    /// Runs the block and proof synchronization loops until one exits unexpectedly.
    pub async fn run(self, shutdown: CancellationToken) -> anyhow::Result<()> {
        let mut tasks = Tasks::new();
        let block_sync = BlockSync {
            state: Arc::clone(&self.state),
            writer: self.block_writer,
            source_rpc: self.source_rpc.clone(),
            readiness: self.readiness,
        };
        let proof_sync = ProofSync {
            state: self.state,
            writer: self.proof_writer,
            source_rpc: self.source_rpc,
        };

        tasks.spawn("block-sync", block_sync.run(shutdown.clone()));
        tasks.spawn("proof-sync", proof_sync.run(shutdown.clone()));

        tasks.join_next_or_cancelled(shutdown).await
    }
}

// SYNC LOOP
// ================================================================================================

struct BlockSync {
    state: Arc<State>,
    writer: BlockWriter,
    source_rpc: RpcClient,
    readiness: RpcReadiness,
}

impl BlockSync {
    async fn run(self, shutdown: CancellationToken) -> anyhow::Result<()> {
        let retry = (|| async {
            self.sync(shutdown.clone())
                .await
                .and_then(|()| Err(anyhow::anyhow!("unexpected end of stream")))
        })
        .retry(retry::constant(RECONNECT_DELAY, None))
        .notify(|err, _| {
            warn!(
                target: LOG_TARGET,
                err = %format!("{err:#}"),
                retry.delay = %RECONNECT_DELAY.as_secs(),
                "Block sync failed, retrying",
            );
        });

        tokio::select! {
            result = retry => result,
            () = shutdown.cancelled() => Ok(()),
        }
    }

    #[miden_instrument(
        target = COMPONENT,
        err,
    )]
    async fn sync(&self, shutdown: CancellationToken) -> anyhow::Result<()> {
        let local_tip = self.state.committed_tip();
        let mut client = self.source_rpc.clone();
        let upstream_tip =
            BlockNumber::from(client.status(tonic::Request::new(())).await?.into_inner().chain_tip);
        self.readiness.update(upstream_tip, local_tip).await;

        let block_from = local_tip.child().as_u32();
        info!(target: LOG_TARGET, block_from, "Connecting to upstream RPC for blocks");

        let mut stream = client
            .block_subscription(BlockSubscriptionRequest { block_from })
            .await?
            .into_inner();

        loop {
            let result = tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                result = stream.next() => result,
            };
            let Some(result) = result else {
                return Ok(());
            };
            let event = result?;
            let upstream_tip = BlockNumber::from(event.committed_chain_tip);
            let block = SignedBlock::read_from_bytes(&event.block)
                .context("failed to deserialize block from upstream")?;
            self.writer.apply_block(block).await?;

            let local_tip = self.state.committed_tip();
            self.readiness.update(upstream_tip, local_tip).await;
        }
    }
}

struct ProofSync {
    state: Arc<State>,
    writer: ProofWriter,
    source_rpc: RpcClient,
}

impl ProofSync {
    /// Synchronizes block proofs from an upstream RPC service.
    ///
    /// Proof sync is intentionally coupled to block sync via the committed tip: a proof is only applied
    /// once its block has been committed locally. This means proof sync can stall if block sync falls
    /// behind, but that is acceptable — there is no value in streaming proofs for blocks that have not
    /// yet been applied.
    async fn run(self, shutdown: CancellationToken) -> anyhow::Result<()> {
        let retry = (|| async {
            self.sync(shutdown.clone())
                .await
                .and_then(|()| Err(anyhow::anyhow!("unexpected end of stream")))
        })
        .retry(retry::constant(RECONNECT_DELAY, None))
        .notify(|err, _| {
            warn!(
                target: LOG_TARGET,
                err = %format!("{err:#}"),
                retry.delay = %RECONNECT_DELAY.as_secs(),
                "Proof sync failed, retrying",
            );
        });

        tokio::select! {
            result = retry => result,
            () = shutdown.cancelled() => Ok(()),
        }
    }

    async fn sync(&self, shutdown: CancellationToken) -> anyhow::Result<()> {
        // Subscribe from next proven tip.
        let starting_block = self.state.proven_tip().child();
        info!(
            target: LOG_TARGET,
            block_from = %starting_block,
            "Subscribing to block proof stream"
        );
        let mut client = self.source_rpc.clone();
        let mut stream = client
            .proof_subscription(ProofSubscriptionRequest { block_from: starting_block.as_u32() })
            .await?
            .into_inner();

        let mut expected = starting_block;
        let mut committed_tip_rx = self.state.subscribe_committed_tip();
        loop {
            let result = tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                result = stream.next() => result,
            };
            let Some(result) = result else {
                return Ok(());
            };
            let event = result?;
            let block_num = BlockNumber::from(event.block_num);

            anyhow::ensure!(
                block_num == expected,
                "upstream sent out-of-sequence proof: expected block {expected}, got {block_num}",
            );

            // Ensure the block is committed before applying its proof so that proven tip never
            // exceeds committed tip.
            tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                result = committed_tip_rx.wait_for(|committed_tip| *committed_tip >= block_num) => {
                    result.context("committed tip channel closed")?;
                },
            }

            self.writer.apply_proof(block_num, event.proof).await?;

            expected = expected.child();
        }
    }
}

#[cfg(test)]
mod readiness_tests {
    use super::*;

    #[test]
    fn readiness_transitions_are_emitted_once_per_state_change() {
        let (reporter, _service) = tonic_health::server::health_reporter();
        let readiness = RpcReadiness::new(reporter, 10);

        assert_eq!(readiness.record_transition(false), ReadinessTransition::InitialNotReady,);
        assert_eq!(readiness.record_transition(false), ReadinessTransition::Unchanged,);
        assert_eq!(readiness.record_transition(true), ReadinessTransition::BecameReady,);
        assert_eq!(readiness.record_transition(true), ReadinessTransition::Unchanged,);
        assert_eq!(readiness.record_transition(false), ReadinessTransition::BecameNotReady,);
    }
}
