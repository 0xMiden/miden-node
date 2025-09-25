use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use futures::TryStreamExt;
use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_proto::domain::mempool::MempoolEvent;
use miden_objects::account::delta::AccountUpdateDetails;
use miden_objects::block::BlockHeader;
use miden_objects::crypto::merkle::PartialMmr;
use miden_objects::transaction::PartialBlockchain;
use tokio::sync::{Barrier, RwLock};
use tokio::time;
use url::Url;

use crate::MAX_IN_PROGRESS_TXS;
use crate::actor::{AccountActorConfig, AccountOrigin};
use crate::block_producer::BlockProducerClient;
use crate::builder::coordinator::Coordinator;
use crate::store::StoreClient;

mod coordinator;

// CONSTANTS
// =================================================================================================

/// The maximum number of blocks to keep in memory while tracking the chain tip.
const MAX_BLOCK_COUNT: usize = 4;

// CHAIN STATE
// ================================================================================================

/// Contains information about the chain that is relevant to the [`NetworkTransactionBuilder`] and
/// all account actors managed by the [`Coordinator`]
#[derive(Debug, Clone)]
pub struct ChainState {
    /// The current tip of the chain.
    pub chain_tip_header: BlockHeader,
    /// A partial representation of the latest state of the chain.
    pub chain_mmr: PartialBlockchain,
}

impl ChainState {
    /// Constructs a new instance of [`ChainState`].
    fn new(chain_tip_header: BlockHeader, chain_mmr: PartialMmr) -> Self {
        let chain_mmr = PartialBlockchain::new(chain_mmr, [])
            .expect("partial blockchain should build from partial mmr");
        Self { chain_tip_header, chain_mmr }
    }

    /// Consumes the chain state and returns the chain tip header and the partial blockchain as a
    /// tuple.
    pub fn into_parts(self) -> (BlockHeader, PartialBlockchain) {
        (self.chain_tip_header, self.chain_mmr)
    }
}

// NETWORK TRANSACTION BUILDER
// ================================================================================================

/// Network transaction builder component.
///
/// The network transaction builder is in in charge of building transactions that consume notes
/// against network accounts. These notes are identified and communicated by the block producer.
/// The service maintains a list of unconsumed notes and periodically executes and proves
/// transactions that consume them (reaching out to the store to retrieve state as necessary).
///
/// The builder manages the tasks for every network account on the chain through the coordinator.
pub struct NetworkTransactionBuilder {
    /// Address of the store gRPC server.
    store_url: Url,
    /// Address of the block producer gRPC server.
    block_producer_url: Url,
    /// Address of the remote prover. If `None`, transactions will be proven locally, which is
    /// undesirable due to the perofmrance impact.
    tx_prover_url: Option<Url>,
    /// Interval for checking pending notes and executing network transactions.
    ticker_interval: Duration,
    /// A checkpoint used to sync start-up process with the block-producer.
    ///
    /// This informs the block-producer when we have subscribed to mempool events and that it is
    /// safe to begin block-production.
    bp_checkpoint: Arc<Barrier>,
    /// Coordinator for managing actor tasks.
    coordinator: Coordinator,
}

impl NetworkTransactionBuilder {
    /// Creates a new instance of the network transaction builder.
    pub fn new(
        store_url: Url,
        block_producer_url: Url,
        tx_prover_url: Option<Url>,
        ticker_interval: Duration,
        bp_checkpoint: Arc<Barrier>,
    ) -> Self {
        Self {
            store_url,
            block_producer_url,
            tx_prover_url,
            ticker_interval,
            bp_checkpoint,
            coordinator: Coordinator::new(MAX_IN_PROGRESS_TXS),
        }
    }

    /// Runs the network transaction builder until a fatal error occurs.
    pub async fn run(mut self) -> anyhow::Result<()> {
        let store = StoreClient::new(self.store_url.clone());
        let block_producer = BlockProducerClient::new(self.block_producer_url.clone());

        let (chain_tip_header, chain_mmr) = store
            .get_latest_blockchain_data_with_retry()
            .await?
            .expect("store should contain a latest block");
        let mut mempool_events = block_producer
            .subscribe_to_mempool_with_retry(chain_tip_header.block_num())
            .await
            .context("failed to subscribe to mempool events")?;

        // Unlock the block-producer's block production. The block-producer is prevented from
        // producing blocks until we have subscribed to mempool events.
        //
        // This is a temporary work-around until the ntb can resync on the fly.
        self.bp_checkpoint.wait().await;

        let mut interval = tokio::time::interval(self.ticker_interval);
        interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

        // Create chain state that will be updated by the coordinator and read by actors.
        let chain_state = Arc::new(RwLock::new(ChainState::new(chain_tip_header, chain_mmr)));
        // Create a semaphore to limit the number of in-progress transactions across all actors.
        let config = AccountActorConfig {
            block_producer_url: self.block_producer_url.clone(),
            tx_prover_url: self.tx_prover_url.clone(),
            chain_state: chain_state.clone(),
            store: store.clone(),
        };

        // Create initial set of actors based on all known network accounts.
        let account_ids = store.get_network_accounts().await?;
        for account_id in account_ids {
            if let Ok(account_prefix) = NetworkAccountPrefix::try_from(account_id) {
                self.coordinator
                    .spawn_actor(AccountOrigin::store(account_prefix), &config)
                    .await;
            }
        }

        // Main loop which manages actors and passes mempool events to them.
        loop {
            tokio::select! {
                // Handle actor result.
                result = self.coordinator.try_next() => {
                    result?;
                },
                // Handle mempool events.
                event = mempool_events.try_next() => {
                    let event = event
                        .context("mempool event stream ended")?
                        .context("mempool event stream failed")?;

                    self.handle_mempool_event(
                        event,
                        &config,
                        chain_state.clone(),
                    ).await;
                },
            }
        }
    }

    /// Handles mempool events by sending them to actors via the coordinator and/or spawning new
    /// actors as required.
    #[tracing::instrument(
        name = "ntx.builder.handle_mempool_event",
        skip(self, event, account_actor_config, chain_state)
    )]
    async fn handle_mempool_event(
        &mut self,
        event: MempoolEvent,
        account_actor_config: &AccountActorConfig,
        chain_state: Arc<RwLock<ChainState>>,
    ) {
        match &event {
            MempoolEvent::TransactionAdded { account_delta, .. } => {
                if let Some(AccountUpdateDetails::New(account)) = account_delta {
                    // Spawn new actors for account creation transactions.
                    if let Some(account) = AccountOrigin::transaction(account) {
                        self.coordinator.spawn_actor(account, account_actor_config).await;
                    }
                } else {
                    // Broadcast event.
                    self.coordinator.broadcast_event(&event);
                }
            },
            // Update chain state and broadcast.
            MempoolEvent::BlockCommitted { header, .. } => {
                self.update_chain_tip(header.clone(), chain_state).await;
                self.coordinator.broadcast_event(&event);
            },
            // Broadcast to all actors.
            MempoolEvent::TransactionsReverted(_) => {
                self.coordinator.broadcast_event(&event);
            },
        }
    }

    /// Updates the chain tip and MMR block count.
    ///
    /// Blocks in the MMR are pruned if the block count exceeds the maximum.
    async fn update_chain_tip(&mut self, tip: BlockHeader, chain_state: Arc<RwLock<ChainState>>) {
        // Lock the chain state.
        let mut chain_state = chain_state.write().await;

        // Update MMR which lags by one block.
        let mmr_tip = chain_state.chain_tip_header.clone();
        chain_state.chain_mmr.add_block(mmr_tip, true);

        // Set the new tip.
        chain_state.chain_tip_header = tip;

        // Keep MMR pruned.
        let pruned_block_height =
            (chain_state.chain_mmr.chain_length().as_usize().saturating_sub(MAX_BLOCK_COUNT))
                as u32;
        chain_state.chain_mmr.prune_to(..pruned_block_height.into());
    }
}
