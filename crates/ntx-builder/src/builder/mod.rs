use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use futures::TryStreamExt;
use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_proto::domain::mempool::MempoolEvent;
use miden_node_proto::domain::note::NetworkNote;
use miden_objects::account::delta::AccountUpdateDetails;
use tokio::sync::{Barrier, Semaphore};
use tokio::task::JoinSet;
use tokio::time;
use url::Url;

use crate::MAX_IN_PROGRESS_TXS;
use crate::actor::{AccountActor, AccountActorConfig, AccountActorError, AccountActorHandle};
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

    actor_registry: HashMap<NetworkAccountPrefix, AccountActorHandle>,
    actor_join_set: JoinSet<Result<(), AccountActorError>>,
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
            actor_registry: HashMap::new(),
            actor_join_set: JoinSet::new(),
        }
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
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
            store_url: self.store_url.clone(),
            block_producer_url: self.block_producer_url.clone(),
            tx_prover_url: self.tx_prover_url.clone(),
            semaphore,
        };

        // Create initial actors for existing accounts.
        let notes = store.get_unconsumed_network_notes().await?;
        // Create initial set of actors based on all available notes.
        for note in notes {
            // Currently only support single target network notes in NTB.
            if let NetworkNote::SingleTarget(note) = note {
                let prefix = note.account_prefix();
                #[allow(clippy::map_entry, reason = "async closure")]
                if !self.actor_registry.contains_key(&prefix) {
                    self.spawn_actor(prefix, &config);
                }
            }
        }

        // Main loop which manages actors and passes mempool events to them.
        loop {
            tokio::select! {
                // Handle actor completion.
                actor_result = self.actor_join_set.join_next() => {
                    match actor_result {
                        Some(Ok(Ok(()))) =>  unreachable!("actor never finishes without error"),
                        Some(Ok(Err(err))) => {
                            match err {
                                AccountActorError::AccountNotFound(prefix) => {
                                    // TODO: Handle account not found error
                                    tracing::error!(prefix = ?prefix, "actor stopped due to account not found");
                                }
                                AccountActorError::StoreError(err) => {
                                    // TODO: Handle store error
                                    tracing::error!(err = ?err, "actor stopped due to store error");
                                }
                                AccountActorError::ChannelClosed => {
                                    // TODO: Handle coordinator channel closed
                                    tracing::error!(err = ?err, "actor stopped due to closed channel");
                                }
                                AccountActorError::SemaphoreError(_) => {
                                    // TODO: Handle semaphore acquisition failure
                                    tracing::error!(err = ?err, "actor stopped due to semaphore error");
                                }
                                AccountActorError::CommittedBlockMismatch{..} => {
                                    // TODO: Handle mismatched committed block
                                    tracing::error!(err = ?err, "actor stopped due to mismatched committed block");
                                }
                                AccountActorError::AccountCreationReverted(prefix) => {
                                    tracing::info!(prefix = ?prefix, "actor stopped due to account creation being reverted");
                                    self.actor_registry.remove(&prefix);
                                }
                            }
                        }
                        Some(Err(err)) => {
                            if err.is_panic() {
                                tracing::error!(err = ?err, "actor join set panicked");
                                // TODO: exit?
                            } else {
                                tracing::error!(err = ?err, "actor join set failed");
                                // TODO: exit?
                            }
                        },
                        None => {
                            tracing::warn!("actor join set is empty");
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                },
                // Handle mempool events.
                event = mempool_events.try_next() => {
                    let event = event
                        .context("mempool event stream ended")?
                        .context("mempool event stream failed")?;

                    self.handle_mempool_event(
                        &event,
                        &config,
                    );
                },
            }
        }
    }

    fn handle_mempool_event(
        &mut self,
        event: &MempoolEvent,
        account_actor_config: &AccountActorConfig,
    ) {
        match &event {
            // Broadcast to affected actors.
            MempoolEvent::TransactionAdded { account_delta, network_notes, .. } => {
                // Find affected accounts.
                let affected_accounts =
                    Self::find_affected_accounts(account_delta.as_ref(), network_notes);

                for account_prefix in affected_accounts {
                    // Retrieve or create the actor.
                    if let Some(actor_handle) = self.actor_registry.get(&account_prefix) {
                        Self::send_event(actor_handle, event.clone());
                    } else {
                        // Try creating the actor.
                        let handle = self.spawn_actor(account_prefix, account_actor_config);
                        Self::send_event(&handle, event.clone());
                    }
                }
            },
            // Broadcast to all actors.
            MempoolEvent::BlockCommitted { .. } | MempoolEvent::TransactionsReverted(_) => {
                self.actor_registry.iter().for_each(|(_, actor_handle)| {
                    // TODO: consider thinner event message.
                    Self::send_event(actor_handle, event.clone());
                });
            },
        }
    }

    fn spawn_actor(
        &mut self,
        account_prefix: NetworkAccountPrefix,
        config: &AccountActorConfig,
    ) -> AccountActorHandle {
        let (actor, handle) = AccountActor::new(account_prefix, config);
        self.actor_registry.insert(account_prefix, handle.clone());
        self.actor_join_set.spawn(async move { actor.run().await });
        handle
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
