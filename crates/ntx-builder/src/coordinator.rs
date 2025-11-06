use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_proto::domain::mempool::MempoolEvent;
use miden_node_proto::domain::note::NetworkNote;
use miden_objects::transaction::TransactionId;
use tokio::sync::mpsc::error::SendError;
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::actor::{AccountActor, AccountActorConfig, AccountOrigin, ActorShutdownReason};

// ACTOR HANDLE
// ================================================================================================

/// Handle to account actors that are spawned by the coordinator.
#[derive(Clone)]
struct ActorHandle {
    event_tx: mpsc::Sender<MempoolEvent>, // TODO: consider Arc<MempoolEvent>.
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
///
/// The `Coordinator` is the central orchestrator of the network transaction builder system.
/// It manages the lifecycle of account actors. Each actor is responsible for handling transactions
/// for a specific network account prefix. The coordinator provides the following core
/// functionality:
///
/// ## Actor Management
/// - Spawns new [`AccountActor`] instances for network accounts as needed.
/// - Maintains a registry of active actors with their communication channels.
/// - Gracefully handles actor shutdown and cleanup when actors complete or fail.
/// - Monitors actor tasks through a join set to detect completion or errors.
///
/// ## Event Broadcasting
/// - Distributes mempool events to all account actors.
/// - Handles communication failures by canceling disconnected actors.
/// - Maintains reliable message delivery through dedicated channels per actor.
///
/// ## Resource Management
/// - Controls transaction concurrency across all network accounts using a semaphore.
/// - Prevents resource exhaustion by limiting simultaneous transaction processing.
///
/// The coordinator operates in an event-driven manner:
/// 1. Network accounts are registered and actors spawned as needed.
/// 2. Mempool events are broadcast to all active actors.
/// 3. Actor completion/failure events are monitored and handled.
/// 4. Failed or completed actors are cleaned up from the registry.
pub struct Coordinator {
    /// Mapping of network account prefixes to their respective message channels and cancellation
    /// tokens.
    ///
    /// This registry serves as the primary directory for communicating with active account actors.
    /// When actors are spawned, they register their communication channel here. When events need
    /// to be broadcast, this registry is used to locate the appropriate actors. The registry is
    /// automatically cleaned up when actors complete their execution.
    actor_registry: HashMap<NetworkAccountPrefix, ActorHandle>,

    /// Join set for managing actor tasks and monitoring their completion status.
    ///
    /// This join set allows the coordinator to wait for actor task completion and handle
    /// different shutdown scenarios. When an actor task completes (either successfully or
    /// due to an error), the corresponding entry is removed from the actor registry.
    actor_join_set: JoinSet<ActorShutdownReason>,

    /// Semaphore for controlling the maximum number of concurrent transactions across all network
    /// accounts.
    ///
    /// This shared semaphore prevents the system from becoming overwhelmed by limiting the total
    /// number of transactions that can be processed simultaneously across all account actors.
    /// Each actor must acquire a permit from this semaphore before processing a transaction,
    /// ensuring fair resource allocation and system stability under load.
    semaphore: Arc<Semaphore>,

    /// Cache of events received from the mempool that predate corresponding network accounts.
    /// Grouped by account prefix to allow targeted event delivery to actors upon creation.
    predating_events: HashMap<NetworkAccountPrefix, HashMap<TransactionId, Arc<MempoolEvent>>>,
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
            predating_events: HashMap::new(),
        }
    }

    /// Spawns a new actor to manage the state of the provided network account.
    ///
    /// This method creates a new [`AccountActor`] instance for the specified account origin
    /// and adds it to the coordinator's management system. The actor will be responsible for
    /// processing transactions and managing state for accounts matching the network prefix.
    #[tracing::instrument(name = "ntx.builder.spawn_actor", skip(self, origin, config))]
    pub async fn spawn_actor(
        &mut self,
        origin: AccountOrigin,
        config: &AccountActorConfig,
    ) -> Result<(), SendError<MempoolEvent>> {
        let account_prefix = origin.prefix();

        // If an actor already exists for this account prefix, something has gone wrong.
        if let Some(handle) = self.actor_registry.remove(&account_prefix) {
            tracing::error!("account actor already exists for prefix: {}", account_prefix);
            handle.cancel_token.cancel();
        }

        // Construct the actor and add it to the registry for subsequent messaging.
        let (event_tx, event_rx) = mpsc::channel(Self::ACTOR_CHANNEL_SIZE);
        let cancel_token = tokio_util::sync::CancellationToken::new();
        let actor = AccountActor::new(origin, config, event_rx, cancel_token.clone());
        let handle = ActorHandle::new(event_tx, cancel_token);

        // Run the actor.
        let semaphore = self.semaphore.clone();
        self.actor_join_set.spawn(Box::pin(actor.run(semaphore)));

        // Send the new actor any events that contain notes that predate account creation.
        if let Some(prefix_events) = self.predating_events.remove(&account_prefix) {
            for event in prefix_events.values() {
                Self::send(&handle, event.clone()).await?;
            }
        }

        self.actor_registry.insert(account_prefix, handle);
        tracing::info!("created actor for account prefix: {}", account_prefix);
        Ok(())
    }

    /// Broadcasts a mempool event to all active account actors.
    ///
    /// This method distributes the provided event to every actor currently registered
    /// with the coordinator. Each actor will receive the event through its dedicated
    /// message channel and can process it accordingly.
    ///
    /// If an actor fails to receive the event, it will be canceled.
    pub async fn broadcast(&mut self, event: Arc<MempoolEvent>) {
        tracing::debug!(
            actor_count = self.actor_registry.len(),
            "broadcasting event to all actors"
        );

        let mut failed_actors = Vec::new();

        // Send event to all actors.
        for (account_prefix, handle) in &self.actor_registry {
            if let Err(err) = Self::send(handle, event.clone()).await {
                tracing::error!("failed to send event to actor {}: {}", account_prefix, err);
                failed_actors.push(*account_prefix);
            }
        }
        // Remove failed actors from registry and cancel them.
        for prefix in failed_actors {
            let handle =
                self.actor_registry.remove(&prefix).expect("actor found in send loop above");
            handle.cancel_token.cancel();
        }
    }

    /// Waits for the next actor to complete and processes the shutdown reason.
    ///
    /// This method monitors the join set for actor task completion and handles
    /// different shutdown scenarios appropriately. It's designed to be called
    /// in a loop to continuously monitor and manage actor lifecycles.
    ///
    /// If no actors are currently running, this method will wait indefinitely until
    /// new actors are spawned. This prevents busy-waiting when the coordinator is idle.
    pub async fn next(&mut self) -> anyhow::Result<()> {
        let actor_result = self.actor_join_set.join_next().await;
        match actor_result {
            Some(Ok(shutdown_reason)) => match shutdown_reason {
                ActorShutdownReason::Cancelled(account_prefix) => {
                    // Do not remove the actor from the registry, as it may be re-spawned.
                    // The coordinator should always remove actors immediately after cancellation.
                    tracing::info!("account actor cancelled: {}", account_prefix);
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

    /// Sends a mempool event to all network account actors that are found in the corresponding
    /// transaction's notes.
    ///
    /// Caches the mempool event for each network account found in the transaction's notes that does
    /// not currently have a corresponding actor.
    ///
    /// If an actor does not exist for the account, it is assumed that the account has not been
    /// created on the chain yet.
    ///
    /// Cached events will be fed to the corresponding actor when the account creation transaction
    /// is processed.
    pub async fn send_targeted(
        &mut self,
        event: &Arc<MempoolEvent>,
    ) -> Result<(), SendError<MempoolEvent>> {
        let mut target_actors = HashMap::new();
        if let MempoolEvent::TransactionAdded { id, network_notes, .. } = event.as_ref() {
            // Determine target actors for each note.
            for note in network_notes {
                if let NetworkNote::SingleTarget(note) = note {
                    let prefix = note.account_prefix();
                    if let Some(actor) = self.actor_registry.get(&prefix) {
                        // Register actor as target.
                        target_actors.insert(prefix, actor);
                    } else {
                        // Cache event for every note that doesn't have a corresponding actor.
                        self.predating_events.entry(prefix).or_default().insert(*id, event.clone());
                    }
                }
            }
        }
        // Send event to target actors.
        for actor in target_actors.values() {
            Self::send(actor, event.clone()).await?;
        }
        Ok(())
    }

    /// Removes any cached events for a given transaction ID from all account prefix caches.
    pub fn drain_predating_events(&mut self, tx_id: &TransactionId) {
        // Remove the transaction from all prefix caches.
        self.predating_events.retain(|_, prefix_event| {
            prefix_event.remove(tx_id);
            // Remove entries for account prefixes with no more cached events.
            !prefix_event.is_empty()
        });
    }

    /// Helper function to send an event to a single account actor.
    async fn send(
        handle: &ActorHandle,
        event: Arc<MempoolEvent>,
    ) -> Result<(), SendError<MempoolEvent>> {
        handle.event_tx.send((*event).clone()).await
    }
}
