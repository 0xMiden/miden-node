use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    num::NonZeroUsize,
};

use account::{AccountState, NetworkAccountUpdate};
use miden_node_proto::domain::{
    account::NetworkAccountPrefix, mempool::MempoolEvent, note::NetworkNote,
};
use miden_objects::{
    account::delta::AccountUpdateDetails, note::Nullifier, transaction::TransactionId,
};

mod account;

/// A candidate network transaction.
///
/// Contains the data pertaining to a specific network account
/// which can be used to build a network transaction.
pub struct Candidate {
    /// The account ID prefix of this network account.
    pub reference: NetworkAccountPrefix,
    /// The current inflight deltas which should be applied to this account.
    ///
    /// Note that the first item _might_ be an account creation update.
    pub _account_deltas: VecDeque<NetworkAccountUpdate>,
    /// A set of notes addressed to this network account.
    pub _notes: Vec<NetworkNote>,
}

/// Holds the state of the network transaction builder.
///
/// It tracks inflight transactions, and their impact on network-related state.
#[derive(Default)]
pub struct State {
    accounts: HashMap<NetworkAccountPrefix, AccountState>,
    in_progress: HashSet<NetworkAccountPrefix>,
    inflight_txs: BTreeMap<TransactionId, Impact>,
    nullifier_idx: BTreeMap<Nullifier, NetworkAccountPrefix>,
}

impl State {
    /// Selects the next candidate network transaction.
    ///
    /// Note that this marks the candidate account as in-progress and that it cannot
    /// be selected again until either:
    ///
    ///   - it has been marked as failed if the transaction failed, or
    ///   - the transaction was submitted successfully, indicated by the associated mempool event
    ///     being submitted
    pub fn select_candidate(&mut self, limit: NonZeroUsize) -> Option<Candidate> {
        // let candidate = self.notes.select()?;
        // let account_deltas = self.accounts.get(candidate).cloned().unwrap_or_default();
        // let notes = self.notes.get(candidate).take(limit.get()).cloned().collect();

        // Some(Candidate {
        //     _account_deltas: account_deltas,
        //     _notes: notes,
        //     reference: candidate,
        // })
        todo!()
    }

    /// Marks a previously selected candidate account as failed, allowing it to be
    /// available for selection again.
    pub fn candidate_failed(&mut self, candidate: NetworkAccountPrefix) {
        // self.notes.deselect(candidate);
    }

    /// Updates state with the mempool event.
    pub fn mempool_update(&mut self, update: MempoolEvent) {
        match update {
            MempoolEvent::TransactionAdded {
                id,
                nullifiers,
                network_notes,
                account_delta,
            } => {
                self.add_transaction(id, nullifiers, network_notes, account_delta);
            },
            MempoolEvent::BlockCommitted { header: _, txs } => {
                for tx in txs {
                    self.commit_transaction(tx);
                }
            },
            MempoolEvent::TransactionsReverted(txs) => {
                for tx in txs {
                    self.revert_transaction(tx);
                }
            },
        }
    }

    /// Handles a [`MempoolEvent::TransactionAdded`] event.
    fn add_transaction(
        &mut self,
        id: TransactionId,
        nullifiers: Vec<Nullifier>,
        network_notes: Vec<NetworkNote>,
        account_delta: Option<AccountUpdateDetails>,
    ) {
        // Skip transactions we already know about.
        //
        // This can occur since both ntx builder and the mempool might
        // inform us of the same transaction. Once when it was submitted to
        // the mempool, and once by the mempool event.
        if self.inflight_txs.contains_key(&id) {
            return;
        }

        let mut tx_impact = Impact::default();
        if let Some(update) = account_delta.map(NetworkAccountUpdate::from_protocol).flatten() {
            tx_impact.account_delta = Some(update.prefix());
            self.accounts.entry(update.prefix()).or_default().add_delta(update);
        }
        for note in network_notes {
            tx_impact.notes.insert(note.nullifier());
            self.nullifier_idx.insert(note.nullifier(), note.account_prefix());
            self.accounts.entry(note.account_prefix()).or_default().add_note(note);
        }
        for nullifier in nullifiers {
            // Ignore nullifiers that aren't network note nullifiers.
            let Some(account) = self.nullifier_idx.get(&nullifier) else {
                continue;
            };
            tx_impact.nullifiers.insert(nullifier);
            self.accounts
                .get_mut(&account)
                .expect("nullifier account must exist")
                .add_nullifier(nullifier);
        }
        self.inflight_txs.insert(id, tx_impact);
    }

    /// Handles [`MempoolEvent::BlockCommitted`] events.
    fn commit_transaction(&mut self, tx: TransactionId) {
        let Impact { account_delta, notes: _, nullifiers } =
            self.inflight_txs.remove(&tx).unwrap_or_default();

        if let Some(prefix) = account_delta {
            self.accounts.get_mut(&prefix).unwrap().commit_delta();
        }

        for nullifier in nullifiers {
            let prefix = self.nullifier_idx.remove(&nullifier).unwrap();
            self.accounts.get_mut(&prefix).unwrap().commit_nullifier(nullifier);
        }
    }

    /// Handles [`MempoolEvent::TransactionsReverted`] events.
    fn revert_transaction(&mut self, tx: TransactionId) {
        let Impact { account_delta, notes, nullifiers } =
            self.inflight_txs.remove(&tx).unwrap_or_default();

        if let Some(prefix) = account_delta {
            self.accounts.get_mut(&prefix).unwrap().revert_delta();
        }

        for note in notes {
            let prefix = self.nullifier_idx.remove(&note).unwrap();
            self.accounts.get_mut(&prefix).unwrap().revert_note(note);
        }

        for nullifier in nullifiers {
            let prefix = self.nullifier_idx.get(&nullifier).unwrap();
            self.accounts.get_mut(prefix).unwrap().revert_nullifier(nullifier);
        }
    }
}

#[derive(Default)]
struct Impact {
    account_delta: Option<NetworkAccountPrefix>,
    notes: BTreeSet<Nullifier>,
    nullifiers: BTreeSet<Nullifier>,
}
