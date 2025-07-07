use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque, hash_map::Entry},
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
/// Contains the data pertaining to a specific network account which can be used to build a network
/// transaction.
pub struct TransactionCandidate {
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
    /// Tracks all network accounts with inflight state.
    ///
    /// This is network account deltas, network notes and their nullifiers.
    accounts: HashMap<NetworkAccountPrefix, AccountState>,

    /// A rotating queue of all tracked network accounts.
    ///
    /// This is used to select the next transaction's account.
    ///
    /// Note that this _always_ includes _all_ network accounts. Filtering out accounts that aren't
    /// viable is handled within the select method itself.
    queue: VecDeque<NetworkAccountPrefix>,

    /// Network accounts which have been selected but whose transaction has not yet completed.
    ///
    /// This locks these accounts so they cannot be selected.
    in_progress: HashSet<NetworkAccountPrefix>,

    /// Uncommitted transactions which have a some impact on the network state.
    ///
    /// This is tracked so we can commit or revert such transaction effects. Transactions _without_
    /// an impact are ignored.
    inflight_txs: BTreeMap<TransactionId, TransactionImpact>,

    /// A mapping of network note's to their account.
    nullifier_idx: BTreeMap<Nullifier, NetworkAccountPrefix>,
}

impl State {
    pub fn with_committed_notes(notes: impl Iterator<Item = NetworkNote>) -> Self {
        let mut state = Self::default();
        for note in notes {
            state.nullifier_idx.insert(note.nullifier(), note.account_prefix());
            state.account_or_default(note.account_prefix()).add_note(note);
        }

        state
    }

    /// Selects the next candidate network transaction.
    ///
    /// Note that this marks the candidate account as in-progress and that it cannot be selected
    /// again until either:
    ///
    ///   - it has been marked as failed if the transaction failed, or
    ///   - the transaction was submitted successfully, indicated by the associated mempool event
    ///     being submitted
    pub fn select_candidate(&mut self, limit: NonZeroUsize) -> Option<TransactionCandidate> {
        // Loop through the account queue until we find one that is selectable.
        //
        // Since the queue contains _all_ accounts, including unselectable accounts, we limit our
        // search to once through the entire queue.
        //
        // There are smarter ways of doing this, but this should scale more than well enough for a
        // long time.
        for _ in 0..self.queue.len() {
            // This is a rotating queue.
            let candidate = self.queue.pop_front().unwrap();
            self.queue.push_back(candidate);

            // Skip accounts which are already in-progress.
            if self.in_progress.contains(&candidate) {
                continue;
            }

            let account = self.accounts.get(&candidate).expect("queue account must be tracked");
            let notes = account.notes().take(limit.get()).cloned().collect::<Vec<_>>();

            // Skip accounts with no available notes.
            if notes.is_empty() {
                continue;
            }

            self.in_progress.insert(candidate);
            return TransactionCandidate {
                reference: candidate,
                _account_deltas: account.deltas().clone(),
                _notes: notes,
            }
            .into();
        }

        None
    }

    /// Marks a previously selected candidate account as failed, allowing it to be available for
    /// selection again.
    pub fn candidate_failed(&mut self, candidate: NetworkAccountPrefix) {
        self.in_progress.remove(&candidate);
    }

    /// Updates state with the mempool event.
    pub fn mempool_update(&mut self, update: MempoolEvent) {
        match update {
            // Note: this event will get triggered by normal user transactions, as well as our
            // network transactions. The mempool does not distinguish between the two.
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
    ///
    /// Note that this will include our own network transactions as well as user submitted
    /// transactions.
    fn add_transaction(
        &mut self,
        id: TransactionId,
        nullifiers: Vec<Nullifier>,
        network_notes: Vec<NetworkNote>,
        account_delta: Option<AccountUpdateDetails>,
    ) {
        // Skip transactions we already know about.
        //
        // This can occur since both ntx builder and the mempool might inform us of the same
        // transaction. Once when it was submitted to the mempool, and once by the mempool event.
        if self.inflight_txs.contains_key(&id) {
            return;
        }

        let mut tx_impact = TransactionImpact::default();
        if let Some(update) = account_delta.and_then(NetworkAccountUpdate::from_protocol) {
            tx_impact.account_delta = Some(update.prefix());
            self.account_or_default(update.prefix()).add_delta(update);
        }
        for note in network_notes {
            tx_impact.notes.insert(note.nullifier());
            self.nullifier_idx.insert(note.nullifier(), note.account_prefix());
            self.account_or_default(note.account_prefix()).add_note(note);
        }
        for nullifier in nullifiers {
            // Ignore nullifiers that aren't network note nullifiers.
            let Some(account) = self.nullifier_idx.get(&nullifier) else {
                continue;
            };
            tx_impact.nullifiers.insert(nullifier);
            // We don't use the entry wrapper here because the account must already exist.
            self.accounts
                .get_mut(account)
                .expect("nullifier account must exist")
                .add_nullifier(nullifier);
        }

        if !tx_impact.is_empty() {
            self.inflight_txs.insert(id, tx_impact);
        }
    }

    /// Grants mutable access to the given account state, creating a default entry if none exists.
    ///
    /// This is effectively a thin wrapper around the entry API, but this also tracks new accounts
    /// to the queue.
    ///
    /// This _must_ be the only way new accounts are added as otherwise they won't be queued.
    fn account_or_default(&mut self, prefix: NetworkAccountPrefix) -> &mut AccountState {
        match self.accounts.entry(prefix) {
            Entry::Occupied(occupied_entry) => occupied_entry.into_mut(),
            Entry::Vacant(vacant_entry) => {
                self.queue.push_back(prefix);
                vacant_entry.insert(AccountState::default())
            },
        }
    }

    /// Handles [`MempoolEvent::BlockCommitted`] events.
    fn commit_transaction(&mut self, tx: TransactionId) {
        // We only track transactions which have an impact on the network state.
        let Some(impact) = self.inflight_txs.remove(&tx) else {
            return;
        };

        if let Some(prefix) = impact.account_delta {
            if self.accounts.get_mut(&prefix).unwrap().commit_delta().is_empty() {
                self.remove_account(prefix);
            }
        }

        for nullifier in impact.nullifiers {
            let prefix = self.nullifier_idx.remove(&nullifier).unwrap();
            if self.accounts.get_mut(&prefix).unwrap().commit_nullifier(nullifier).is_empty() {
                self.remove_account(prefix);
            }
        }
    }

    /// Handles [`MempoolEvent::TransactionsReverted`] events.
    fn revert_transaction(&mut self, tx: TransactionId) {
        // We only track transactions which have an impact on the network state.
        let Some(impact) = self.inflight_txs.remove(&tx) else {
            return;
        };

        if let Some(prefix) = impact.account_delta {
            if self.accounts.get_mut(&prefix).unwrap().revert_delta().is_empty() {
                self.remove_account(prefix);
            }
        }

        for note in impact.notes {
            let prefix = self.nullifier_idx.remove(&note).unwrap();
            if self.accounts.get_mut(&prefix).unwrap().revert_note(note).is_empty() {
                self.remove_account(prefix);
            }
        }

        for nullifier in impact.nullifiers {
            let prefix = self.nullifier_idx.get(&nullifier).unwrap();
            self.accounts.get_mut(prefix).unwrap().revert_nullifier(nullifier);
        }
    }

    /// Removes the account from tracking under the assumption that it is empty.
    fn remove_account(&mut self, prefix: NetworkAccountPrefix) {
        // We don't need to prune the inflight transactions because if the account is empty, then it
        // would have no inflight txs.
        self.accounts.remove(&prefix);
        self.queue.retain(|x| x != &prefix);
    }
}

/// The impact a transaction has on the state.
#[derive(Default)]
struct TransactionImpact {
    /// The network account this transaction added an account delta to.
    account_delta: Option<NetworkAccountPrefix>,

    /// Network notes this transaction created.
    notes: BTreeSet<Nullifier>,

    /// Network notes this transaction consumed.
    nullifiers: BTreeSet<Nullifier>,
}

impl TransactionImpact {
    fn is_empty(&self) -> bool {
        self.account_delta.is_none() && self.notes.is_empty() && self.nullifiers.is_empty()
    }
}
