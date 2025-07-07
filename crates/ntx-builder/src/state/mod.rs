use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque, hash_map::Entry},
    num::NonZeroUsize,
};

use account::{AccountState, NetworkAccountUpdate};
use anyhow::Context;
use miden_node_proto::domain::{
    account::NetworkAccountPrefix, mempool::MempoolEvent, note::NetworkNote,
};
use miden_objects::{
    account::{Account, delta::AccountUpdateDetails},
    note::Nullifier,
    transaction::TransactionId,
};

use crate::store::{StoreClient, StoreError};

mod account;

/// A candidate network transaction.
///
/// Contains the data pertaining to a specific network account which can be used to build a network
/// transaction.
pub struct TransactionCandidate {
    /// The current inflight state of the account.
    pub account: Account,
    /// A set of notes addressed to this network account.
    pub _notes: Vec<NetworkNote>,
}

/// Holds the state of the network transaction builder.
///
/// It tracks inflight transactions, and their impact on network-related state.
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

    /// gRPC client used to retrieve the network account state from the store.
    store: StoreClient,
}

impl State {
    /// Load's all available network notes from the store, along with the required account states.
    pub async fn load(store: StoreClient) -> Result<Self, StoreError> {
        let mut state = Self {
            store,
            accounts: HashMap::default(),
            queue: VecDeque::default(),
            in_progress: HashSet::default(),
            inflight_txs: BTreeMap::default(),
            nullifier_idx: BTreeMap::default(),
        };

        let notes = state.store.get_unconsumed_network_notes().await?;
        for note in notes {
            let prefix = note.account_prefix();

            // Ignore notes which don't target an existing account.
            let Some(account) = state.account_or_load(prefix).await? else {
                continue;
            };
            account.add_note(note);
        }

        Ok(state)
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

            // Skip empty accounts, and prune them.
            //
            // This is how we keep the number of accounts bounded.
            if account.is_empty() {
                // We don't need to prune the inflight transactions because if the account is empty,
                // then it would have no inflight txs.
                self.accounts.remove(&candidate);
                // We know this account is the backmost one since we just rotated it there.
                self.queue.pop_back();
                continue;
            }

            let notes = account.notes().take(limit.get()).cloned().collect::<Vec<_>>();

            // Skip accounts with no available notes.
            if notes.is_empty() {
                continue;
            }

            self.in_progress.insert(candidate);
            return TransactionCandidate {
                account: account.account(),
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
    pub async fn mempool_update(&mut self, update: MempoolEvent) -> anyhow::Result<()> {
        match update {
            // Note: this event will get triggered by normal user transactions, as well as our
            // network transactions. The mempool does not distinguish between the two.
            MempoolEvent::TransactionAdded {
                id,
                nullifiers,
                network_notes,
                account_delta,
            } => {
                self.add_transaction(id, nullifiers, network_notes, account_delta).await?;
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

        Ok(())
    }

    /// Handles a [`MempoolEvent::TransactionAdded`] event.
    ///
    /// Note that this will include our own network transactions as well as user submitted
    /// transactions.
    async fn add_transaction(
        &mut self,
        id: TransactionId,
        nullifiers: Vec<Nullifier>,
        network_notes: Vec<NetworkNote>,
        account_delta: Option<AccountUpdateDetails>,
    ) -> anyhow::Result<()> {
        // Skip transactions we already know about.
        //
        // This can occur since both ntx builder and the mempool might inform us of the same
        // transaction. Once when it was submitted to the mempool, and once by the mempool event.
        if self.inflight_txs.contains_key(&id) {
            return Ok(());
        }

        let mut tx_impact = TransactionImpact::default();
        if let Some(update) = account_delta.and_then(NetworkAccountUpdate::from_protocol) {
            let prefix = update.prefix();
            match update {
                NetworkAccountUpdate::New(account) => {
                    let account_state = AccountState::uncommitted(account);
                    self.accounts.insert(prefix, account_state);
                    self.queue.push_back(prefix);
                },
                NetworkAccountUpdate::Delta(account_delta) => {
                    self.account_or_load(prefix)
                        .await
                        .context("failed to load account")?
                        .context("account with delta not found")?
                        .add_delta(&account_delta);
                },
            }

            tx_impact.account_delta = Some(prefix);
        }
        for note in network_notes {
            tx_impact.notes.insert(note.nullifier());
            self.nullifier_idx.insert(note.nullifier(), note.account_prefix());
            // Skip notes which target a non-existent network account.
            if let Some(account) = self
                .account_or_load(note.account_prefix())
                .await
                .context("failed to load account")?
            {
                account.add_note(note);
            }
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

        Ok(())
    }

    /// Handles [`MempoolEvent::BlockCommitted`] events.
    fn commit_transaction(&mut self, tx: TransactionId) {
        // We only track transactions which have an impact on the network state.
        let Some(impact) = self.inflight_txs.remove(&tx) else {
            return;
        };

        if let Some(prefix) = impact.account_delta {
            self.accounts.get_mut(&prefix).unwrap().commit_delta();
        }

        for nullifier in impact.nullifiers {
            let prefix = self.nullifier_idx.remove(&nullifier).unwrap();
            // Its possible for the account to no longer exist if the transaction creating it was
            // reverted.
            if let Some(account) = self.accounts.get_mut(&prefix) {
                account.commit_nullifier(nullifier);
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
            // We need to remove the account if this transaction created the account.
            if self.accounts.get_mut(&prefix).unwrap().revert_delta() {
                self.accounts.remove(&prefix);
            }
        }

        for note in impact.notes {
            let prefix = self.nullifier_idx.remove(&note).unwrap();
            // Its possible for the account to no longer exist if the transaction creating it was
            // reverted.
            if let Some(account) = self.accounts.get_mut(&prefix) {
                account.revert_note(note);
            }
        }

        for nullifier in impact.nullifiers {
            let prefix = self.nullifier_idx.get(&nullifier).unwrap();
            // Its possible for the account to no longer exist if the transaction creating it was
            // reverted.
            if let Some(account) = self.accounts.get_mut(prefix) {
                account.revert_nullifier(nullifier);
            }
        }
    }

    /// Returns the current inflight account, loading it from the store if it isn't present locally.
    ///
    /// Returns `None` if the account is unknown.
    async fn account_or_load(
        &mut self,
        prefix: NetworkAccountPrefix,
    ) -> Result<Option<&mut AccountState>, StoreError> {
        match self.accounts.entry(prefix) {
            Entry::Occupied(occupied_entry) => Ok(Some(occupied_entry.into_mut())),
            Entry::Vacant(vacant_entry) => {
                let Some(account) = self.store.get_network_account(prefix).await? else {
                    return Ok(None);
                };

                self.queue.push_back(prefix);
                let entry = vacant_entry.insert(AccountState::committed(account));

                Ok(Some(entry))
            },
        }
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
