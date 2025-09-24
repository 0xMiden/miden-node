use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_proto::domain::mempool::MempoolEvent;
use miden_objects::account::Account;
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;

use crate::actor::{AccountActor, AccountActorConfig, ActorShutdownReason};
use crate::state::State;
use crate::store::StoreClient;

// COORDINATOR
// ================================================================================================

/// Coordinator for managing [`AccountActor`] instances, tasks, and associated communication.
pub struct Coordinator {
    /// Mapping of network account prefixes to their respective message channels. When actors are
    /// spawned, this registry is updated. The builder uses this registry to communicate with the
    /// actors.
    actor_registry: HashMap<NetworkAccountPrefix, mpsc::UnboundedSender<MempoolEvent>>,
    /// Join set for managing actor tasks. When an actor task completes, the actor's corresponding
    /// data from the registry is removed.
    actor_join_set: JoinSet<ActorShutdownReason>,
    /// Semaphore for limiting the number of concurrent transactions across all network accounts.
    semaphore: Arc<Semaphore>,
}

impl Coordinator {
    /// Creates a new coordinator with the specified maximum number of inflight transactions.
    pub fn new(max_inflight_transactions: usize) -> Self {
        Self {
            actor_registry: HashMap::new(),
            actor_join_set: JoinSet::new(),
            semaphore: Arc::new(Semaphore::new(max_inflight_transactions)),
        }
    }

    /// Spawns a new actor to manage the state of the provided network account.
    ///
    /// If the account is not a network account, the function returns Ok(()) without spawning an
    /// actor.
    #[tracing::instrument(name = "ntx.builder.spawn_actor", skip(self, account, config, store))]
    pub async fn spawn_actor(
        &mut self,
        account: Account,
        config: &AccountActorConfig,
        store: StoreClient,
    ) -> anyhow::Result<()> {
        // Only spawn actors for network accounts.
        let Ok(account_prefix) = NetworkAccountPrefix::try_from(account.id()) else {
            return Ok(());
        };

        // Load the account state from the store.
        let block_num = config.chain_state.read().await.chain_tip_header.block_num();
        let state = State::load(account, account_prefix, store, block_num).await?;

        // Construct the actor and add it to the registry for subsequent messaging.
        let (actor, event_tx) = AccountActor::new(config);
        self.actor_registry.insert(account_prefix, event_tx.clone());

        // Run the actor.
        let semaphore = self.semaphore.clone();
        self.actor_join_set.spawn(async move { actor.run(state, semaphore).await });

        tracing::info!("created actor for account prefix: {}", account_prefix);
        Ok(())
    }

    /// Broadcasts an event to all account actors.
    pub fn broadcast_event(&self, event: &MempoolEvent) {
        self.actor_registry.iter().for_each(|(account_prefix, event_tx)| {
            Self::send(event_tx, event, *account_prefix);
        });
    }

    /// Tries to get the next result from the actor join set and then handles it depending on the
    /// reason the actor shutdown.
    pub async fn try_next(&mut self) -> anyhow::Result<()> {
        let actor_result = self.actor_join_set.join_next().await;
        match actor_result {
            Some(Ok(shutdown_reason)) => match shutdown_reason {
                ActorShutdownReason::AccountReverted(account_prefix) => {
                    tracing::info!("account reverted: {}", account_prefix);
                    self.actor_registry.remove(&account_prefix);
                    Ok(())
                },
                ActorShutdownReason::EventChannelClosed => {
                    anyhow::bail!("event channel closed");
                },
                ActorShutdownReason::SemaphoreFailed(err) => Err(err).context("semaphore failed"),
            },
            Some(Err(err)) => {
                if err.is_panic() {
                    Err(err).context("actor join set panicked")
                } else {
                    Err(err).context("actor join set failed")
                }
            },
            None => {
                // There are no actors to wait for. Wait indefinitely until actors are spawned.
                std::future::pending().await
            },
        }
    }

    /// Helper function to send an event to a single account actor.
    fn send(
        event_tx: &mpsc::UnboundedSender<MempoolEvent>,
        event: &MempoolEvent,
        account_prefix: NetworkAccountPrefix,
    ) {
        if let Err(error) = event_tx.send(event.clone()) {
            tracing::warn!(
                account = %account_prefix,
                error = ?error,
                "actor channel disconnected"
            );
        }
    }
}
