use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Mul,
};

use miden_node_proto::domain::mempool::MempoolEvent;
use miden_objects::{
    block::{BlockHeader, BlockNumber},
    transaction::{OutputNote, TransactionId},
};
use tokio::sync::mpsc;

use crate::domain::transaction::AuthenticatedTransaction;

#[derive(Default, Clone, Debug)]
pub(crate) struct SubscriptionProvider {
    /// The latest event subscription, if any.
    ///
    /// The only current interested party is the network transaction builder, so one subscription is enough.
    subscription: Option<mpsc::Sender<MempoolEvent>>,

    /// The latest committed block number.
    ///
    /// This is used to ensure synchronicity with new subscribers.
    chain_tip: BlockNumber,

    /// [`MempoolEvent::TransactionAdded`] events which are still inflight i.e. have not been committed or reverted.
    ///
    /// These events need to be transmitted when a subscription is started, since the subscriber only has the committed state.
    ///
    /// A [`BTreeMap`] is used to maintain event ordering while allowing for efficient removals of committed or reverted transactions.
    ///
    /// The key is auto-incremented on each new insert to support this event ordering.
    ///
    /// A reverse lookup index is maintained in `uncommitted_txs_index`.
    uncommitted_txs: BTreeMap<u64, MempoolEvent>,

    /// A reverse lookup index for `uncommitted_txs` which allows for efficient removal of committed or reverted events.
    uncommitted_txs_index: BTreeMap<TransactionId, u64>,
}

impl SubscriptionProvider {
    /// Creates a new [`MempoolEvent`] subscription.
    ///
    /// This replaces any existing subscription.
    ///
    /// # Errors
    ///
    /// Returns an error if the provided chain tip does not match the provider's. The error
    /// value contains the provider's chain tip.
    ///
    /// This prevents desync between the subscribers view of the world and the mempool's event
    /// stream.
    pub fn subscribe(
        &mut self,
        chain_tip: BlockNumber,
    ) -> Result<mpsc::Receiver<MempoolEvent>, BlockNumber> {
        if self.chain_tip != chain_tip {
            return Err(self.chain_tip);
        }

        // We should leave enough space to at least send the uncommitted events (plus some extra).
        let capacity = self.uncommitted_txs.len().mul(2).max(1024);
        let (tx, rx) = mpsc::channel(capacity);
        self.subscription.replace(tx);

        // Send each uncommited tx event in chronological order.
        //
        // The ordering is guaranteed by the btree map so we can safely
        // iterate over the values.
        for tx in self.uncommitted_txs.values() {
            Self::send_event(&mut self.subscription, tx.clone());
        }

        Ok(rx)
    }

    pub(super) fn transaction_added(&mut self, tx: &AuthenticatedTransaction) {
        let id = tx.id();
        let nullifiers = tx.nullifiers().collect();
        let network_notes = tx
            .output_notes()
            .filter_map(|note| match note {
                OutputNote::Full(inner) if inner.is_network_note() => Some(inner),
                _ => None,
            })
            .cloned()
            .collect();
        let account_delta =
            tx.account_id().is_network().then(|| tx.account_update().details().clone());
        let event = MempoolEvent::TransactionAdded {
            id,
            nullifiers,
            network_notes,
            account_delta,
        };

        // Add the event to the uncommitted set.
        //
        // The key increments to maintain chronological ordering.
        let key = self.uncommitted_txs.last_key_value().map(|(k, _v)| k + 1).unwrap_or_default();
        self.uncommitted_txs_index.insert(id, key);
        self.uncommitted_txs.insert(key, event.clone());

        Self::send_event(&mut self.subscription, event);
    }

    pub(super) fn block_committed(&mut self, header: BlockHeader, txs: Vec<TransactionId>) {
        self.chain_tip = header.block_num();
        self.decommit_txs(&txs);
        Self::send_event(&mut self.subscription, MempoolEvent::BlockCommitted { header, txs });
    }

    pub(super) fn txs_reverted(&mut self, txs: BTreeSet<TransactionId>) {
        self.decommit_txs(&txs);
        Self::send_event(&mut self.subscription, MempoolEvent::TransactionsReverted(txs));
    }

    /// Sends a [`MempoolEvent`] to the subscriber, if any.
    ///
    /// If the send fails, then the subscription is cancelled.
    fn send_event(subscription: &mut Option<mpsc::Sender<MempoolEvent>>, event: MempoolEvent) {
        let Some(sender) = subscription else {
            return;
        };

        // If sending fails, end the subscription to prevent desync.
        if let Err(error) = sender.try_send(event) {
            tracing::warn!(%error, "mempool subscription failed, cancelling subscription");
            subscription.take();
        }
    }

    /// Removes the transactions from the uncommitted transactions set.
    fn decommit_txs<'a>(&mut self, txs: impl IntoIterator<Item = &'a TransactionId>) {
        for tx in txs {
            let Some(idx) = self.uncommitted_txs_index.remove(tx) else {
                tracing::error!(%tx, "inflight transaction not found in subscription index");
                continue;
            };
            self.uncommitted_txs.remove(&idx);
        }
    }
}
