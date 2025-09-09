use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use futures::TryStreamExt;
use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_proto::domain::note::NetworkNote;
use miden_objects::account::delta::AccountUpdateDetails;
use miden_remote_prover_client::remote_prover::tx_prover::RemoteTransactionProver;
use tokio::sync::{Barrier, Semaphore};
use tokio::time;
use url::Url;

use crate::MAX_IN_PROGRESS_TXS;
use crate::actor::{AccountActor, AccountConfig, ActorHandle, CoordinatorMessage};
use crate::block_producer::BlockProducerClient;
use crate::state::State;
use crate::store::StoreClient;
use crate::transaction::NtxContext;

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
    #[allow(clippy::too_many_lines)]
    pub async fn serve_new(self) -> anyhow::Result<()> {
        let store = StoreClient::new(self.store_url);
        let block_producer = BlockProducerClient::new(self.block_producer_url);

        let state = State::load(store.clone()).await.context("failed to load ntx state")?;

        let mut mempool_events = block_producer
            .subscribe_to_mempool_with_retry(state.chain_tip())
            .await
            .context("failed to subscribe to mempool events")?;

        // Unlock the block-producer's block production. The block-producer is prevented from
        // producing blocks until we have subscribed to mempool events.
        //
        // This is a temporary work-around until the ntb can resync on the fly.
        self.bp_checkpoint.wait().await;

        let prover = self.tx_prover_url.map(RemoteTransactionProver::new);

        let mut interval = tokio::time::interval(self.ticker_interval);
        interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

        // Set up actors and communications.
        //let (actor_msg_tx, mut actor_msg_rx) = mpsc::unbounded_channel::<ActorMessage>();
        let mut actor_registry = HashMap::<NetworkAccountPrefix, ActorHandle>::new();
        let rate_limiter = Arc::new(Semaphore::new(MAX_IN_PROGRESS_TXS));

        let ntx_context = NtxContext {
            block_producer: block_producer.clone(),
            prover,
        };

        let default_config = AccountConfig { max_note_attempts: 10 };

        // Create initial actors for existing accounts
        for (account_prefix, _account_state) in state.accounts().iter() {
            let actor_handle = AccountActor::spawn(
                *account_prefix,
                None, // State will be initialized when first transaction is processed
                ntx_context.clone(),
                rate_limiter.clone(),
                default_config.clone(),
                state.chain_tip(),
            );
            actor_registry.insert(*account_prefix, actor_handle);
        }

        loop {
            tokio::select! {
                // Periodic tick - broadcast to all actors
                _tick = interval.tick() => {
                    for (account_prefix, actor_handle) in &actor_registry {
                        if actor_handle.is_finished() {
                            tracing::error!(
                                account = %account_prefix,
                                "Actor finished unexpectedly, will respawn on next event"
                            );
                            continue;
                        }

                        if let Err(error) = actor_handle.send(CoordinatorMessage::ProcessTick) {
                            tracing::error!(
                                account = %account_prefix,
                                error = ?error,
                                "Failed to send tick to actor"
                            );
                        }
                    }
                },

                // Handle mempool events
                event = mempool_events.try_next() => {
                    let event = event
                                .context("mempool event stream ended")?
                                .context("mempool event stream failed")?;

                    match &event {
                        miden_node_proto::domain::mempool::MempoolEvent::TransactionAdded {
                            account_delta,
                            network_notes,
                            ..
                        } => {
                            // Route to affected accounts
                            let mut affected_accounts = std::collections::HashSet::new();

                            // Check if any account deltas affect our tracked accounts
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

                                    // Create new actor.
                                    actor_registry.entry(prefix).or_insert_with( || {
                                        AccountActor::spawn(
                                            prefix,
                                            None, // State will be initialized by the actor when it receives the delta
                                            ntx_context.clone(),
                                            rate_limiter.clone(),
                                            default_config.clone(),
                                            state.chain_tip(),
                                        )
                                    });
                                }
                            }

                            // Check which accounts are affected by network notes
                            for note in network_notes {
                                if let NetworkNote::SingleTarget(note) = note {
                                    let prefix = note.account_prefix();
                                    affected_accounts.insert(prefix);

                                    // Create new actor.
                                    actor_registry.entry(prefix).or_insert_with( || {
                                        AccountActor::spawn(
                                            prefix,
                                            None, // State will be initialized by the actor when it receives the delta
                                            ntx_context.clone(),
                                            rate_limiter.clone(),
                                            default_config.clone(),
                                            state.chain_tip(),
                                        )
                                    });
                                }
                            }

                            // Send event to all affected actors
                            for account_prefix in affected_accounts {
                                if let Some(actor_handle) = actor_registry.get(&account_prefix) {
                                    if let Err(error) = actor_handle.send(CoordinatorMessage::MempoolEvent(event.clone())) {
                                        tracing::error!(
                                            account = %account_prefix,
                                            error = ?error,
                                            "Failed to send mempool event to actor"
                                        );
                                    }
                                } else {
                                    tracing::error!(
                                        account = %account_prefix,
                                        "Failed to find actor handle for mempool event"
                                    );
                                }
                            }
                        },

                        miden_node_proto::domain::mempool::MempoolEvent::BlockCommitted { .. } |
                        miden_node_proto::domain::mempool::MempoolEvent::TransactionsReverted(_) => {
                            // Broadcast to all actors
                            for (account_prefix, actor_handle) in &actor_registry {
                                if let Err(error) = actor_handle.send(CoordinatorMessage::MempoolEvent(event.clone())) {
                                    tracing::error!(
                                        account = %account_prefix,
                                        error = ?error,
                                        "Failed to send mempool event to actor"
                                    );
                                }
                            }
                        }
                    }
                },

            }
        }
    }
}
