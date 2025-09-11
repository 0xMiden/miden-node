use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use futures::TryStreamExt;
use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_proto::domain::mempool::MempoolEvent;
use miden_node_proto::domain::note::NetworkNote;
use miden_objects::account::delta::AccountUpdateDetails;
use tokio::sync::Barrier;
use tokio::time;
use url::Url;

use crate::actor::{AccountActor, AccountActorConfig, AccountActorHandle, CoordinatorMessage};
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

        let config = AccountActorConfig {
            tick_interval_ms: self.ticker_interval, // todo these should be separate values?
            store_url: self.store_url,
            block_producer_url: self.block_producer_url,
            tx_prover_url: self.tx_prover_url,
        };

        // Create initial actors for existing accounts.
        let mut actor_registry = HashMap::<NetworkAccountPrefix, AccountActorHandle>::new();
        let notes = store.get_unconsumed_network_notes().await?;
        for note in notes {
            // Currently only support single target network notes in NTB.
            if let NetworkNote::SingleTarget(note) = note {
                let prefix = note.account_prefix();
                if !actor_registry.contains_key(&prefix) {
                    let actor = AccountActor::spawn(prefix, config.clone())
                        .await
                        .expect("todo, could panic here");
                    actor_registry.insert(prefix, actor);
                }
            }
        }

        loop {
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
                        config.clone(),
                        &state,
                    )?;
                },
            }
        }
    }

    /// Handles mempool events by routing them to affected account actors.
    fn handle_mempool_event(
        event_result: Result<Option<MempoolEvent>, tonic::Status>,
        actor_registry: &mut HashMap<NetworkAccountPrefix, AccountActorHandle>,
        account_actor_config: AccountActorConfig,
        state: &crate::state::State,
    ) -> anyhow::Result<()> {
        let event = event_result
            .context("mempool event stream ended")?
            .context("mempool event stream failed")?;

        match &event {
            // Broadcast to affected actors.
            MempoolEvent::TransactionAdded { account_delta, network_notes, .. } => {
                // Find affected accounts.
                let affected_accounts =
                    Self::find_affected_accounts(&account_delta, &network_notes);

                for account_prefix in affected_accounts {
                    // Update registry - create actor if it doesn't exist.
                    if !actor_registry.contains_key(&account_prefix) {
                        let actor_handle = AccountActor::spawn(
                            account_prefix,
                            state.clone(),
                            context.clone(),
                            AccountActorConfig::default(),
                        );
                        actor_registry.insert(account_prefix, actor_handle);
                    }

                    // Send event.
                    let actor_handle =
                        actor_registry.get(&account_prefix).expect("actor insertion is inevitable");
                    if let Err(error) =
                        actor_handle.send(CoordinatorMessage::MempoolEvent(event.clone()))
                    {
                        tracing::error!(
                            account = %account_prefix,
                            error = ?error,
                            "Failed to send mempool event to actor"
                        );
                    }
                }
            },
            // Broadcast to all actors.
            MempoolEvent::BlockCommitted { .. } | MempoolEvent::TransactionsReverted(_) => {
                for (account_prefix, actor_handle) in actor_registry {
                    if let Err(error) =
                        actor_handle.send(CoordinatorMessage::MempoolEvent(event.clone()))
                    {
                        tracing::error!(
                            account = %account_prefix,
                            error = ?error,
                            "Failed to send mempool event to actor"
                        );
                    }
                }
            },
        };

        Ok(())
    }

    fn find_affected_accounts(
        account_delta: &Option<AccountUpdateDetails>,
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
