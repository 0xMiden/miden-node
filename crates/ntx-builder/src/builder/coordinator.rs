use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_proto::domain::mempool::MempoolEvent;
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::actor::{AccountActor, AccountActorConfig, AccountOrigin, ActorShutdownReason};

// ACTOR HANDLE
// ================================================================================================

/// Handle to account actors that are spawned by the coordinator.
#[derive(Clone)]
struct ActorHandle {
    event_tx: mpsc::Sender<MempoolEvent>,
    cancel_token: CancellationToken,
}

impl ActorHandle {
    fn new(event_tx: mpsc::Sender<MempoolEvent>, cancel_token: CancellationToken) -> Self {
        Self { event_tx, cancel_token }
    }
}

// COORDINATOR
// ================================================================================================

/// Coordinator for managing [`AccountActor`] instances, tasks, and associated communication.
pub struct Coordinator {
    /// Mapping of network account prefixes to their respective message channels. When actors are
    /// spawned, this registry is updated. The builder uses this registry to communicate with the
    /// actors.
    actor_registry: HashMap<NetworkAccountPrefix, ActorHandle>,
    /// Join set for managing actor tasks. When an actor task completes, the actor's corresponding
    /// data from the registry is removed.
    actor_join_set: JoinSet<ActorShutdownReason>,
    /// Semaphore for limiting the number of concurrent transactions across all network accounts.
    semaphore: Arc<Semaphore>,
}

impl Coordinator {
    /// Maximum number of messages of the message channel for each actor.
    const ACTOR_CHANNEL_SIZE: usize = 100;

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
    #[tracing::instrument(name = "ntx.builder.spawn_actor", skip(self, origin, config))]
    pub async fn spawn_actor(&mut self, origin: AccountOrigin, config: &AccountActorConfig) {
        let account_prefix = origin.prefix();

        // If an actor already exists for this account prefix, something has gone wrong.
        if let Some(handle) = self.actor_registry.get(&account_prefix) {
            tracing::error!("account actor already exists for prefix: {}", account_prefix);
            handle.cancel_token.cancel();
        }

        // Construct the actor and add it to the registry for subsequent messaging.
        let (event_tx, event_rx) = mpsc::channel(Self::ACTOR_CHANNEL_SIZE);
        let cancel_token = tokio_util::sync::CancellationToken::new();
        let actor = AccountActor::new(origin, config, event_rx, cancel_token.clone());
        let handle = ActorHandle::new(event_tx, cancel_token);
        self.actor_registry.insert(account_prefix, handle);

        // Run the actor.
        let semaphore = self.semaphore.clone();
        self.actor_join_set.spawn(Box::pin(actor.run(semaphore)));

        tracing::info!("created actor for account prefix: {}", account_prefix);
    }

    /// Broadcasts an event to all account actors.
    pub async fn broadcast_event(&self, event: &MempoolEvent) {
        for (account_prefix, handle) in &self.actor_registry {
            Self::send(handle, event, *account_prefix).await;
        }
    }

    /// Gets the next result from the actor join set and then handles it depending on the
    /// reason the actor shutdown.
    pub async fn next(&mut self) -> anyhow::Result<()> {
        let actor_result = self.actor_join_set.join_next().await;
        match actor_result {
            Some(Ok(shutdown_reason)) => match shutdown_reason {
                ActorShutdownReason::Cancelled(account_prefix) => {
                    tracing::info!("account actor cancelled: {}", account_prefix);
                    self.actor_registry.remove(&account_prefix);
                    Ok(())
                },
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
                tracing::error!(err = %err, "actor task failed");
                Ok(())
            },
            None => {
                // There are no actors to wait for. Wait indefinitely until actors are spawned.
                std::future::pending().await
            },
        }
    }

    /// Helper function to send an event to a single account actor.
    async fn send(
        handle: &ActorHandle,
        event: &MempoolEvent,
        account_prefix: NetworkAccountPrefix,
    ) {
        if let Err(error) = handle.event_tx.send(event.clone()).await {
            tracing::warn!(
                account = %account_prefix,
                error = ?error,
                "actor channel disconnected"
            );
            handle.cancel_token.cancel();
        }
    }
}
