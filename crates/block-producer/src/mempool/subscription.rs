use std::collections::BTreeSet;

use miden_node_proto::domain::{mempool::MempoolEvent, note::NetworkNote};
use miden_objects::{
    block::{BlockHeader, BlockNumber},
    note::NoteExecutionMode,
    transaction::{OutputNote, TransactionId},
};
use tokio::sync::mpsc;

use crate::domain::transaction::AuthenticatedTransaction;

#[derive(Default, Clone, Debug)]
pub(crate) struct SubscriptionProvider {
    /// The latest event subscription, if any.
    ///
    /// We only allow a single active subscription since we only have a single interested party.
    subscription: Option<mpsc::Sender<MempoolEvent>>,

    /// The latest committed block number.
    ///
    /// This is used to ensure synchronicity with new subscribers.
    chain_tip: BlockNumber,
}

impl SubscriptionProvider {
    /// Creates a new [`MempoolEvent`] subscription.
    ///
    /// This replaces any existing subscription.
    ///
    /// # Errors
    ///
    /// Returns an error if the provided chain_tip does not match the provider's. The error
    /// value contains the provider's chain tip.
    ///
    /// This prevents desync between the subscribers view of the world and the mempool's event stream.
    pub fn subscribe(
        &mut self,
        chain_tip: BlockNumber,
    ) -> Result<mpsc::Receiver<MempoolEvent>, BlockNumber> {
        if self.chain_tip != chain_tip {
            return Err(self.chain_tip);
        }

        let (tx, rx) = mpsc::channel(1024);

        self.subscription.replace(tx);
        Ok(rx)
    }

    pub(super) fn transaction_added(&mut self, tx: &AuthenticatedTransaction) {
        let id = tx.id();
        let nullifiers = tx.nullifiers().collect();
        let network_notes = tx
            .output_notes()
            .filter_map(|note| match note {
                OutputNote::Full(inner)
                    if inner.metadata().tag().execution_mode() == NoteExecutionMode::Network =>
                {
                    NetworkNote::try_from(inner.clone()).ok()
                },
                _ => None,
            })
            .collect();
        let account_delta =
            tx.account_id().is_network().then(|| tx.account_update().details().clone());
        let event = MempoolEvent::TransactionAdded {
            id,
            nullifiers,
            network_notes,
            account_delta,
        };

        self.send_event(event);
    }

    pub(super) fn block_committed(&mut self, header: BlockHeader, txs: Vec<TransactionId>) {
        self.chain_tip = header.block_num();
        self.send_event(MempoolEvent::BlockCommitted { header, txs });
    }

    pub(super) fn txs_reverted(&mut self, txs: BTreeSet<TransactionId>) {
        self.send_event(MempoolEvent::TransactionsReverted(txs));
    }

    /// Sends a [`MempoolEvent`] to the subscriber, if any.
    ///
    /// If the send fails, then the subscription is cancelled.
    fn send_event(&mut self, event: MempoolEvent) {
        let Some(subscription) = &mut self.subscription else {
            return;
        };

        if let Err(error) = subscription.try_send(event) {
            tracing::warn!(%error, "mempool subscription failed, cancelling subscription");
            self.subscription = None;
        }
    }
}
