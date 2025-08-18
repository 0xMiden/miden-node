use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use futures::TryStreamExt;
use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_utils::ErrorReport;
use miden_remote_prover_client::remote_prover::tx_prover::RemoteTransactionProver;
use tokio::sync::Barrier;
use tokio::time;
use url::Url;

use crate::MAX_IN_PROGRESS_TXS;
use crate::block_producer::BlockProducerClient;
use crate::store::StoreClient;

// NETWORK TRANSACTION BUILDER
// ================================================================================================

/// Network transaction builder component.
///
/// The network transaction builder is in in charge of building transactions that consume notes
/// against network accounts. These notes are identified and communicated by the block producer.
/// The service maintains a list of unconsumed notes and periodically executes and proves
/// transactions that consume them (reaching out to the store to retrieve state as necessary).
pub struct NetworkTransactionBuilder {
    /// Address of the store gRPC server.
    pub store_url: Url,
    /// Address of the block producer gRPC server.
    pub block_producer_url: Url,
    /// Address of the remote prover. If `None`, transactions will be proven locally, which is
    /// undesirable due to the perofmrance impact.
    pub tx_prover_url: Option<Url>,
    /// Interval for checking pending notes and executing network transactions.
    pub ticker_interval: Duration,
    /// A checkpoint used to sync start-up process with the block-producer.
    ///
    /// This informs the block-producer when we have subscribed to mempool events and that it is
    /// safe to begin block-production.
    pub bp_checkpoint: Arc<Barrier>,
}

impl NetworkTransactionBuilder {
    pub async fn serve_new(self) -> anyhow::Result<()> {
        let store = StoreClient::new(&self.store_url);
        let block_producer = BlockProducerClient::new(&self.block_producer_url);

        let mut state = crate::state::State::load(store.clone())
            .await
            .context("failed to load ntx state")?;

        let mut mempool_events = block_producer
            .subscribe_to_mempool_with_retry(state.chain_tip())
            .await
            .context("failed to subscribe to mempool events")?;

        // Unlock the block-producer's block production. The block-producer is prevented from
        // producing blocks until we have subscribed to mempool events.
        //
        // This is a temporary work-around until the ntb can resync on the fly.
        self.bp_checkpoint.wait().await;

        let prover = self.tx_prover_url.map(RemoteTransactionProver::new);

        let mut interval = tokio::time::interval(self.ticker_interval);
        interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

        // Tracks network transaction tasks until they are submitted to the mempool.
        //
        // We also map the task ID to the network account so we can mark it as failed if it doesn't
        // get submitted.
        let mut inflight = JoinSet::new();
        let mut inflight_idx = HashMap::new();

        let context = crate::transaction::NtxContext {
            block_producer: block_producer.clone(),
            prover,
        };

        loop {
            tokio::select! {
                _next = interval.tick() => {
                    if inflight.len() > MAX_IN_PROGRESS_TXS {
                        tracing::info!("At maximum network tx capacity, skipping");
                        continue;
                    }

                    let Some(candidate) = state.select_candidate(crate::MAX_NOTES_PER_TX) else {
                        tracing::debug!("No candidate network transaction available");
                        continue;
                    };

                    let prefix = NetworkAccountPrefix::try_from(candidate.account.id()).unwrap();
                    let task_id = inflight.spawn({
                        let context = context.clone();
                        context.execute_transaction(candidate)
                    }).id();

                    // SAFETY: This is definitely a network account.
                    inflight_idx.insert(task_id, prefix);
                },
                event = mempool_events.try_next() => {
                    let event = event
                        .context("mempool event stream ended")?
                        .context("mempool event stream failed")?;
                    state.mempool_update(event).await.context("failed to update state")?;
                },
                completed = inflight.join_next_with_id() => {
                    // Grab the task ID and associated network account reference.
                    let task_id = match &completed {
                        Ok((task_id, _)) => *task_id,
                        Err(join_handle) => join_handle.id(),
                    };
                    // SAFETY: both inflights should have the same set.
                    let candidate = inflight_idx.remove(&task_id).unwrap();

                    match completed {
                        // Nothing to do. State will be updated by the eventual mempool event.
                        Ok((_, Ok(_))) => {},
                        // Inform state if the tx failed.
                        Ok((_, Err(err))) => {
                            tracing::warn!(err=err.as_report(), "network transaction failed");
                            state.candidate_failed(candidate);
                        },
                        Err(err) => {
                            tracing::warn!(err=err.as_report(), "network transaction panic'd");
                            state.candidate_failed(candidate);
                        }
                    }
                }
            }
        }
    }
}

/// A wrapper arounnd tokio's [`JoinSet`](tokio::task::JoinSet) which returns pending instead of
/// [`None`] if its empty.
///
/// This makes it much more convenient to use in a `select!`.
struct JoinSet<T>(tokio::task::JoinSet<T>);

impl<T> JoinSet<T>
where
    T: 'static,
{
    fn new() -> Self {
        Self(tokio::task::JoinSet::new())
    }

    fn spawn<F>(&mut self, task: F) -> tokio::task::AbortHandle
    where
        F: Future<Output = T>,
        F: Send + 'static,
        T: Send,
    {
        self.0.spawn(task)
    }

    async fn join_next_with_id(&mut self) -> Result<(tokio::task::Id, T), tokio::task::JoinError> {
        if self.0.is_empty() {
            std::future::pending().await
        } else {
            // Cannot be None as its not empty.
            self.0.join_next_with_id().await.unwrap()
        }
    }

    fn len(&self) -> usize {
        self.0.len()
    }
}
