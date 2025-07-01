use std::{collections::VecDeque, num::NonZeroUsize};

use account::NetworkAccountUpdate;
use miden_node_proto::domain::{
    account::NetworkAccountPrefix, mempool::MempoolEvent, note::NetworkNote,
};

mod account;
mod note;

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
pub struct State {
    accounts: account::AccountDeltas,
    notes: note::Notes,
}

impl State {
    pub fn new(notes: impl Iterator<Item = NetworkNote>) -> Self {
        Self {
            notes: note::Notes::new(notes),
            accounts: account::AccountDeltas::default(),
        }
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
                // If a network account was updated, then we also need to mark
                // it as available for selection again.
                if let Some(update) = account_delta {
                    if let Some(account) = self.accounts.add(id, update) {
                        self.notes.deselect(account);
                    }
                }

                self.notes.add(id, network_notes, nullifiers);
            },
            MempoolEvent::BlockCommitted { header: _, txs } => {
                for tx in txs {
                    self.accounts.commit(&tx);
                    self.notes.commit(&tx);
                }
            },
            MempoolEvent::TransactionsReverted(txs) => {
                for tx in txs {
                    self.accounts.revert(&tx);
                    self.notes.revert(&tx);
                }
            },
        }
    }

    /// Selects the next candidate network transaction.
    ///
    /// Note that this marks the candidate account as in-progress and that it cannot
    /// be selected again until either:
    ///
    ///   - it has been marked as failed if the transaction failed, or
    ///   - the transaction was submitted successfully, indicated by the associated mempool event
    ///     being submitted
    pub fn select_candidate(&mut self, limit: NonZeroUsize) -> Option<Candidate> {
        let candidate = self.notes.select()?;
        let account_deltas = self.accounts.get(candidate).cloned().unwrap_or_default();
        let notes = self.notes.get(candidate).take(limit.get()).cloned().collect();

        Some(Candidate {
            _account_deltas: account_deltas,
            _notes: notes,
            reference: candidate,
        })
    }

    /// Marks a previously selected candidate account as failed, allowing it to be
    /// available for selection again.
    pub fn candidate_failed(&mut self, candidate: NetworkAccountPrefix) {
        self.notes.deselect(candidate);
    }
}
