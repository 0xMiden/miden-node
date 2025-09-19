use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use futures::TryStreamExt;
use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_proto::domain::mempool::MempoolEvent;
use miden_node_proto::domain::note::NetworkNote;
use miden_objects::account::delta::AccountUpdateDetails;
use miden_objects::block::BlockHeader;
use miden_objects::crypto::merkle::PartialMmr;
use miden_objects::transaction::PartialBlockchain;
use tokio::sync::{Barrier, RwLock, Semaphore};
use tokio::time;
use url::Url;

use crate::MAX_IN_PROGRESS_TXS;
use crate::actor::AccountActorConfig;
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

#[derive(Debug, Clone)]
pub struct ChainState {
    pub chain_tip_header: Arc<RwLock<BlockHeader>>,
    pub chain_mmr: Arc<RwLock<PartialBlockchain>>,
}

impl ChainState {
    fn new(chain_tip_header: BlockHeader, chain_mmr: PartialMmr) -> Self {
        let chain_mmr = PartialBlockchain::new(chain_mmr, [])
            .expect("partial blockchain should build from partial mmr");
        Self {
            chain_tip_header: RwLock::new(chain_tip_header).into(),
            chain_mmr: RwLock::new(chain_mmr).into(),
        }
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
            coordinator: Coordinator::default(),
        }
    }

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
        let chain_state = ChainState::new(chain_tip_header, chain_mmr);
        // Create a semaphore to limit the number of in-progress transactions across all actors.
        let semaphore = Arc::new(Semaphore::new(MAX_IN_PROGRESS_TXS));
        let config = AccountActorConfig {
            block_producer_url: self.block_producer_url.clone(),
            tx_prover_url: self.tx_prover_url.clone(),
            semaphore,
            chain_state: chain_state.clone(),
        };

        // Create initial actors for existing accounts.
        let accounts = store.get_network_accounts().await?;
        // Create initial set of actors based on all network accounts.
        for account in accounts {
            let account_prefix = NetworkAccountPrefix::try_from(account.id())
                .expect("store endpoint only returns network accounts");
            #[allow(clippy::map_entry, reason = "async closure")]
            if !self.coordinator.contains(account_prefix) {
                let account = store.get_network_account(account_prefix).await?;
                if let Some(account) = account {
                    self.coordinator
                        .spawn_actor(account, account_prefix, &config, store.clone())
                        .await?;
                    tracing::info!("created initial actor for account prefix: {}", account_prefix);
                }
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
                        &event,
                        &config,
                        store.clone(),
                        chain_state.clone(),
                    ).await?;
                },
            }
        }
    }

    #[tracing::instrument(
        name = "ntx.builder.handle_mempool_event",
        skip(self, event, account_actor_config, store, chain_state)
    )]
    async fn handle_mempool_event(
        &mut self,
        event: &MempoolEvent,
        account_actor_config: &AccountActorConfig,
        store: StoreClient,
        chain_state: ChainState,
    ) -> anyhow::Result<()> {
        match &event {
            // Spawn new actors for account creation transactions.
            MempoolEvent::TransactionAdded { account_delta, network_notes, .. } => {
                if let Some(AccountUpdateDetails::New(account)) = account_delta {
                    // Handle account creation.
                    if let Ok(account_prefix) = NetworkAccountPrefix::try_from(account.id()) {
                        self.coordinator
                            .spawn_actor(
                                account.clone(),
                                account_prefix,
                                account_actor_config,
                                store.clone(),
                            )
                            .await?;
                        tracing::info!("created new actor for account prefix: {}", account_prefix);
                    }
                    Ok(())
                } else {
                    let affected_accounts =
                        Self::find_affected_accounts(account_delta.as_ref(), network_notes);

                    for account_prefix in affected_accounts {
                        self.coordinator.send_event(account_prefix, event.clone());
                    }
                    Ok(())
                }
            },
            // Update chain state and do not broadcast.
            MempoolEvent::BlockCommitted { header, .. } => {
                self.update_chain_tip(header.clone(), chain_state).await;
                // TODO: should we send this through?
                self.coordinator.broadcast_event(event);
                Ok(())
            },
            // Broadcast to all actors.
            MempoolEvent::TransactionsReverted(_) => {
                self.coordinator.broadcast_event(event);
                Ok(())
            },
        }
    }

    /// Updates the chain tip and MMR block count.
    ///
    /// Blocks in the MMR are pruned if the block count exceeds the maximum.
    async fn update_chain_tip(&mut self, tip: BlockHeader, chain_state: ChainState) {
        // Update MMR which lags by one block.
        let mut chain_mmr = chain_state.chain_mmr.write().await;
        let mut chain_tip_header = chain_state.chain_tip_header.write().await;
        chain_mmr.add_block(chain_tip_header.clone(), true);

        // Set the new tip.
        *chain_tip_header = tip;

        // Keep MMR pruned.
        let pruned_block_height =
            (chain_mmr.chain_length().as_usize().saturating_sub(MAX_BLOCK_COUNT)) as u32;
        chain_mmr.prune_to(..pruned_block_height.into());
    }

    fn find_affected_accounts(
        account_delta: Option<&AccountUpdateDetails>,
        network_notes: &[NetworkNote],
    ) -> HashSet<NetworkAccountPrefix> {
        let mut affected_accounts = HashSet::new();

        // Find affected accounts from account delta.
        if let Some(delta) = account_delta {
            let account_prefix = match delta {
                AccountUpdateDetails::Delta(delta) => {
                    NetworkAccountPrefix::try_from(delta.id()).ok()
                },
                // New accounts are handled elsewhere. Private accounts are not handled at all.
                AccountUpdateDetails::New(_) | AccountUpdateDetails::Private => None,
            };

            if let Some(prefix) = account_prefix {
                affected_accounts.insert(prefix);
            }
        }

        // Find affected accounts from network notes.
        for note in network_notes {
            if let NetworkNote::SingleTarget(note) = note {
                affected_accounts.insert(note.account_prefix());
            }
        }

        affected_accounts
    }
}
