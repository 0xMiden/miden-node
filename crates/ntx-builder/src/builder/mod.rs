use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use futures::TryStreamExt;
use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_proto::domain::mempool::MempoolEvent;
use miden_node_proto::domain::note::NetworkNote;
use miden_objects::account::delta::AccountUpdateDetails;
use tokio::sync::{Barrier, Semaphore};
use tokio::time;
use url::Url;

use crate::MAX_IN_PROGRESS_TXS;
use crate::actor::{AccountActor, AccountActorConfig, AccountActorHandle};
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
        let store = StoreClient::new(self.store_url.clone());
        let block_producer = BlockProducerClient::new(self.block_producer_url.clone());

        let (chain_tip_header, _) = store
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

        let semaphore = Arc::new(Semaphore::new(MAX_IN_PROGRESS_TXS));
        let config = AccountActorConfig {
            store_url: self.store_url,
            block_producer_url: self.block_producer_url,
            tx_prover_url: self.tx_prover_url,
            semaphore,
        };

        // Create initial actors for existing accounts.
        let mut actor_registry = HashMap::new();
        let mut actor_removal_queue = VecDeque::new();
        let notes = store.get_unconsumed_network_notes().await?;
        for note in notes {
            // Currently only support single target network notes in NTB.
            if let NetworkNote::SingleTarget(note) = note {
                let prefix = note.account_prefix();
                let account = store.get_network_account(prefix).await?;
                if let Some(account) = account {
                    #[allow(clippy::map_entry, reason = "async closure")]
                    if !actor_registry.contains_key(&prefix) {
                        let actor = AccountActor::spawn(prefix, account, config.clone()).await?;
                        actor_registry.insert(prefix, actor);
                    }
                }
            }
        }

        loop {
            // Remove actors that have been marked for removal from the registry.
            for prefix in actor_removal_queue.drain(..) {
                let actor_handle =
                    actor_registry.remove(&prefix).expect("actor must exist for removal");
                actor_handle.abort();
            }

            tokio::select! {
                _tick = interval.tick() => {
                    for (account_prefix, actor_handle) in &actor_registry {
                        if actor_handle.is_finished() {
                            tracing::error!(
                                account = %account_prefix,
                                "actor finished unexpectedly, will respawn on next event"
                            );
                        }
                    }
                },
                event = mempool_events.try_next() => {
                    Self::handle_mempool_event(
                        event,
                        &mut actor_registry,
                        &mut actor_removal_queue,
                        config.clone(),
                        &store,
                    ).await?;
                },
            }
        }
    }

    /// Handles mempool events by routing them to affected account actors.
    async fn handle_mempool_event(
        event_result: Result<Option<MempoolEvent>, tonic::Status>,
        actor_registry: &mut HashMap<NetworkAccountPrefix, AccountActorHandle>,
        actor_removal_queue: &mut VecDeque<NetworkAccountPrefix>,
        account_actor_config: AccountActorConfig,
        store: &StoreClient,
    ) -> anyhow::Result<()> {
        let event = event_result
            .context("mempool event stream ended")?
            .context("mempool event stream failed")?;

        match &event {
            // Broadcast to affected actors.
            MempoolEvent::TransactionAdded { account_delta, network_notes, .. } => {
                // Find affected accounts.
                let affected_accounts =
                    Self::find_affected_accounts(account_delta.as_ref(), network_notes);

                for account_prefix in affected_accounts {
                    // Update registry - create actor if it doesn't exist.
                    #[allow(clippy::map_entry, reason = "async closure")]
                    if !actor_registry.contains_key(&account_prefix) {
                        let account = store.get_network_account(account_prefix).await?;
                        if let Some(account) = account {
                            let actor_handle = AccountActor::spawn(
                                account_prefix,
                                account,
                                account_actor_config.clone(),
                            )
                            .await?;
                            actor_registry.insert(account_prefix, actor_handle);
                        }
                    }

                    // Send event.
                    let actor_handle = actor_registry
                        .get(&account_prefix)
                        .expect("actor previously existed or inserted above");
                    // TODO: consider thinner event message.
                    if let Err(error) = actor_handle.send(event.clone()) {
                        tracing::warn!(
                            account = %actor_handle.account_prefix,
                            error = ?error,
                            "actor channel disconnected"
                        );
                        actor_removal_queue.push_back(account_prefix);
                    }
                }
            },
            // Broadcast to all actors.
            MempoolEvent::BlockCommitted { .. } | MempoolEvent::TransactionsReverted(_) => {
                for (prefix, actor_handle) in actor_registry.iter() {
                    // TODO: consider thinner event message.
                    if let Err(error) = actor_handle.send(event.clone()) {
                        tracing::warn!(
                            account = %actor_handle.account_prefix,
                            error = ?error,
                            "actor channel disconnected"
                        );
                        actor_removal_queue.push_back(*prefix);
                    }
                }
            },
        }

        Ok(())
    }

    fn find_affected_accounts(
        account_delta: Option<&AccountUpdateDetails>,
        network_notes: &[NetworkNote],
    ) -> HashSet<NetworkAccountPrefix> {
        let mut affected_accounts = HashSet::new();

        // Find affected accounts from account delta.
        if let Some(delta) = account_delta {
            let account_prefix = match delta {
                AccountUpdateDetails::New(account) => {
                    NetworkAccountPrefix::try_from(account.id()).ok()
                },
                AccountUpdateDetails::Delta(delta) => {
                    NetworkAccountPrefix::try_from(delta.id()).ok()
                },
                AccountUpdateDetails::Private => None,
            };

            if let Some(prefix) = account_prefix {
                affected_accounts.insert(prefix);
            }
        }

        // Find affected accounts from network notes.
        for note in network_notes {
            if let NetworkNote::SingleTarget(note) = note {
                let prefix = note.account_prefix();
                affected_accounts.insert(prefix);
            }
        }

        affected_accounts
    }
}
