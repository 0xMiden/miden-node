use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ops::Mul,
    sync::{Arc, Weak},
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
    /// We only allow a single active subscription since we only have a single interested party.
    subscription: Option<mpsc::Sender<MempoolEvent>>,

    /// The latest committed block number.
    ///
    /// This is used to ensure synchronicity with new subscribers.
    chain_tip: BlockNumber,

    /// Transaction event's which have not yet been committed or reverted.
    ///
    /// The `ordered_txs` field keeps track of the chronological ordering.
    uncommitted_txs: BTreeMap<TransactionId, Arc<MempoolEvent>>,

    /// Chronologically ordered transaction events which have not yet been committed or reverted.
    ///
    /// These need to be sent for new subscriptions since otherwise these events would be skipped.
    ///
    /// These events are stored as weak pointers to the actual data to allow for efficient removal
    /// without moving the entire queue.
    ///
    /// The queue itself is trimmed by removing non-existent pointers whenever from the front and
    /// back whenever txs are committed or reverted. This does mean slow or long-term txs let
    /// the queue build up in size.
    ordered_txs: VecDeque<Weak<MempoolEvent>>,
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

        // Send each uncommited tx event.
        //
        // TODO: Figure out a better solution without the clone.
        for tx in self.ordered_txs.clone() {
            let Some(tx) = tx.upgrade() else {
                continue;
            };

            self.send_event(tx.as_ref().clone());
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

        let uncommitted_event = Arc::new(event.clone());
        self.ordered_txs.push_back(Arc::downgrade(&uncommitted_event));
        self.uncommitted_txs.insert(id, uncommitted_event);
        self.send_event(event);
    }

    pub(super) fn block_committed(&mut self, header: BlockHeader, txs: Vec<TransactionId>) {
        self.chain_tip = header.block_num();
        self.decommit_txs(&txs);
        self.send_event(MempoolEvent::BlockCommitted { header, txs });
    }

    pub(super) fn txs_reverted(&mut self, txs: BTreeSet<TransactionId>) {
        self.decommit_txs(&txs);
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

    fn decommit_txs<'a>(&mut self, txs: impl IntoIterator<Item = &'a TransactionId>) {
        for tx in txs {
            self.uncommitted_txs.remove(tx);
        }

        // Attempt to drop as many non-existent txs from the queue.
        while let Some(front) = self.ordered_txs.front() {
            if front.upgrade().is_none() {
                self.ordered_txs.pop_front();
            }
        }
        while let Some(back) = self.ordered_txs.back() {
            if back.upgrade().is_none() {
                self.ordered_txs.pop_back();
            }
        }
    }
}
