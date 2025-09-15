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
use tokio::task::{JoinError, JoinHandle, JoinSet};
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
    /// ... todo
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

        // Create a semaphore to limit the number of in-progress transactions across all actors.
        let semaphore = Arc::new(Semaphore::new(MAX_IN_PROGRESS_TXS));
        let config = AccountActorConfig {
            store_url: self.store_url,
            block_producer_url: self.block_producer_url,
            tx_prover_url: self.tx_prover_url,
            semaphore,
        };

        // Create initial actors for existing accounts.
        let mut actor_registry = HashMap::new();
        let notes = store.get_unconsumed_network_notes().await?;
        // Create initial set of actors based on all available notes.
        for note in notes {
            // Currently only support single target network notes in NTB.
            if let NetworkNote::SingleTarget(note) = note {
                let prefix = note.account_prefix();
                let account = store.get_network_account(prefix).await?; // TODO: should we add a batch endpoint for this?
                if let Some(account) = account {
                    #[allow(clippy::map_entry, reason = "async closure")]
                    if !actor_registry.contains_key(&prefix) {
                        let (actor, handle) =
                            AccountActor::new(prefix, account, config.clone()).await?;
                        actor_registry.insert(prefix, handle);
                    }
                }
            }
        }

        // Main loop which manages actors and passes mempool events to them.
        let actor_join_set = JoinSet::new();
        loop {
            tokio::select! {
                // Handle actor completion.
                actor_result = actor_join_set.join_next() => {
                    match actor_result {
                        Some(Ok(Ok(()))) =>  unreachable!("actor never finishes without error"),
                        Some(Ok(Err(err))) => {
                            // ...
                        }
                        Some(Err(err)) => {
                            if err.is_panic() {
                                tracing::error!("actor join set panicked: {:?}", err);
                            } else {
                                tracing::error!("actor join set failed: {:?}", err);
                            }
                        },
                        None => {
                            tracing::error!("actor join set unexpectedly empty");
                        }
                    }
                    for (account_prefix, actor_handle) in &actor_registry {
                        if actor_handle.is_finished() {
                            tracing::error!(
                                account = %account_prefix,
                                "actor finished unexpectedly, will respawn on next event"
                            );
                        }
                    }
                },
                // Handle mempool events.
                event = mempool_events.try_next() => {
                    let event = event
                        .context("mempool event stream ended")?
                        .context("mempool event stream failed")?;

                    Self::handle_mempool_event(
                        event,
                        &mut actor_registry,
                        &mut actor_join_set,
                        config.clone(),
                        &store,
                    ).await?;
                },
            }
        }
    }

    async fn handle_mempool_event(
        event: MempoolEvent,
        actor_registry: &mut HashMap<NetworkAccountPrefix, AccountActorHandle>,
        actor_join_set: &mut JoinSet<anyhow::Result<()>>,
        account_actor_config: AccountActorConfig,
        store: &StoreClient,
    ) -> anyhow::Result<()> {
        match &event {
            // Broadcast to affected actors.
            MempoolEvent::TransactionAdded { account_delta, network_notes, .. } => {
                // Find affected accounts.
                let affected_accounts =
                    Self::find_affected_accounts(account_delta.as_ref(), network_notes);

                for account_prefix in affected_accounts {
                    // Retrieve or create the actor.
                    if let Some(actor_handle) = actor_registry.get(&account_prefix) {
                        Self::send_event(actor_handle, event.clone());
                    } else {
                        // Try creating the actor.
                        let account = store.get_network_account(account_prefix).await?;
                        if let Some(account) = account {
                            let (actor, handle) = AccountActor::new(
                                account_prefix,
                                account,
                                account_actor_config.clone(),
                            )
                            .await?;
                            // TODO: consider thinner event message.
                            actor_join_set.spawn(async move { actor.run().await });
                            actor_registry.insert(account_prefix, handle);
                            Self::send_event(&handle, event.clone());
                        } else {
                            tracing::warn!("network account {account_prefix} not found");
                        }
                    }
                }
            },
            // Broadcast to all actors.
            MempoolEvent::BlockCommitted { .. } | MempoolEvent::TransactionsReverted(_) => {
                for (_, actor_handle) in actor_registry.iter() {
                    // TODO: consider thinner event message.
                    Self::send_event(actor_handle, event.clone(), actor_removal_queue);
                }
            },
        }

        Ok(())
    }

    /// Sends an event to an actor handle and queues it for removal if the channel is disconnected.
    fn send_event(actor_handle: &AccountActorHandle, event: MempoolEvent) {
        if let Err(error) = actor_handle.send(event) {
            tracing::warn!(
                account = %actor_handle.account_prefix,
                error = ?error,
                "actor channel disconnected"
            );
        }
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
                affected_accounts.insert(note.account_prefix());
            }
        }

        affected_accounts
    }
}
