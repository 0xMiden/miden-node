use std::{collections::HashMap, net::SocketAddr, num::NonZeroUsize, time::Duration};

use anyhow::Context;
use futures::TryStreamExt;
use miden_node_proto::domain::account::NetworkAccountPrefix;
use tokio::time;
use url::Url;

use crate::{MAX_IN_PROGRESS_TXS, block_producer::BlockProducerClient, store::StoreClient};

// NETWORK TRANSACTION BUILDER
// ================================================================================================

/// Network transaction builder component.
///
/// The network transaction builder is in in charge of building transactions that consume notes
/// against network accounts. These notes are identified and communicated by the block producer.
/// The service maintains a list of unconsumed notes and periodically executes and proves
/// transactions that consume them (reaching out to the store to retrieve state as necessary).
pub struct NetworkTransactionBuilder {
    /// The address for the network transaction builder gRPC server.
    pub ntx_builder_address: SocketAddr,
    /// Address of the store gRPC server.
    pub store_url: Url,
    /// Address of the block producer gRPC server.
    pub block_producer_address: SocketAddr,
    /// Address of the remote prover. If `None`, transactions will be proven locally, which is
    /// undesirable due to the perofmrance impact.
    pub tx_prover_url: Option<Url>,
    /// Interval for checking pending notes and executing network transactions.
    pub ticker_interval: Duration,
    /// Capacity of the in-memory account cache for the executor's data store.
    pub account_cache_capacity: NonZeroUsize,
}

impl NetworkTransactionBuilder {
    pub async fn serve_new(self) -> anyhow::Result<()> {
        // TODO: separate out the startup stuff so it can loop and repeat and wait on the network
        // etc.
        let store = StoreClient::new(&self.store_url);
        let block_prod = BlockProducerClient::new(self.block_producer_address);

        let mut state = crate::state::State::load(store.clone())
            .await
            .context("failed to load ntx state")?;

        let (chain_tip, _mmr) = store
            .get_current_blockchain_data(None)
            .await
            .context("failed to fetch the chain tip data from the store")?
            .context("chain tip data was None")?;

        let mut mempool_events = block_prod
            .subscribe_to_mempool(chain_tip.block_num())
            .await
            .context("failed to subscribe to mempool events")?;

        let mut interval = tokio::time::interval(self.ticker_interval);
        interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

        // Tracks network transaction tasks until they are submitted to the mempool.
        //
        // We also map the task ID to the network account so we can mark it as failed if it doesn't
        // get submitted.
        let mut inflight = JoinSet::new();
        let mut inflight_idx = HashMap::new();

        loop {
            tokio::select! {
                _next = interval.tick() => {
                    if inflight.len() > MAX_IN_PROGRESS_TXS {
                        tracing::info!("At maximum network tx capacity, skipping");
                        continue;
                    }

                    let Some(candidate) = state.select_candidate(crate::MAX_NOTES_PER_TX) else {
                        tracing::info!("No candidate network transaction available");
                        continue;
                    };

                    let task_id = inflight.spawn(async move {
                        // TODO: Run the actual tx.

                        Result::<_, ()>::Ok(())
                    }).id();

                    // SAFETY: This is definitely a network account.
                    let prefix = NetworkAccountPrefix::try_from(candidate.account.id()).unwrap();
                    inflight_idx.insert(task_id, prefix);
                },
                event = mempool_events.try_next() => {
                    let event = event.context("mempool event stream ended")?.context("mempool event stream failed")?;
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
                        Ok((_, Err(_))) | Err(_) => state.candidate_failed(candidate),
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
