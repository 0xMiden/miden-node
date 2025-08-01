use std::collections::{HashMap, VecDeque};

use miden_node_proto::domain::{account::NetworkAccountPrefix, note::NetworkNote};
use miden_objects::{
    account::{Account, AccountDelta, AccountId, delta::AccountUpdateDetails},
    block::BlockNumber,
    note::Nullifier,
};

// INFLIGHT NETWORK NOTE
// ================================================================================================

/// An unconsumed network note that may have failed to execute.
///
/// The block number at which the network note was attempted are approximate and may not
/// reflect the exact block number for which the execution attempt failed. The actual block
/// will likely be soon after the number that is recorded here.
#[derive(Debug, Clone)]
pub struct InflightNetworkNote {
    note: NetworkNote,
    attempt_count: usize,
    last_attempt: Option<BlockNumber>,
}

impl InflightNetworkNote {
    /// Creates a new inflight network note.
    pub fn new(note: NetworkNote) -> Self {
        Self {
            note,
            attempt_count: 0,
            last_attempt: None,
        }
    }

    /// Consumes the inflight network note and returns the inner network note.
    pub fn into_inner(self) -> NetworkNote {
        self.note
    }

    /// Returns a reference to the inner network note.
    pub fn to_inner(&self) -> &NetworkNote {
        &self.note
    }

    /// Returns the number of attempts made to execute the network note.
    pub fn attempt_count(&self) -> usize {
        self.attempt_count
    }

    /// Returns the last block number at which the network note was attempted.
    pub fn last_attempt(&self) -> Option<BlockNumber> {
        self.last_attempt
    }

    /// Registers a failed attempt to execute the network note at the specified block number.
    pub fn fail(&mut self, block_num: BlockNumber) {
        self.last_attempt = Some(block_num);
        self.attempt_count += 1;
    }
}

// ACCOUNT STATE
// ================================================================================================

/// Tracks the state of a network account and its notes.
pub struct AccountState {
    /// The committed account state, if any.
    ///
    /// Its possible this is `None` if the account creation transaction is still inflight.
    committed: Option<Account>,

    /// Inflight account updates in chronological order.
    inflight: VecDeque<Account>,

    /// Unconsumed notes of this account.
    available_notes: HashMap<Nullifier, InflightNetworkNote>,

    /// Notes which have been consumed by transactions that are still inflight.
    nullified_notes: HashMap<Nullifier, NetworkNote>,
}

impl AccountState {
    /// Creates a new account state using the given value as the committed state.
    pub fn from_committed_account(account: Account) -> Self {
        Self {
            committed: Some(account),
            inflight: VecDeque::default(),
            available_notes: HashMap::default(),
            nullified_notes: HashMap::default(),
        }
    }

    /// Creates a new account state where the creating transaction is still inflight.
    pub fn from_uncommitted_account(account: Account) -> Self {
        Self {
            inflight: VecDeque::from([account]),
            committed: None,
            available_notes: HashMap::default(),
            nullified_notes: HashMap::default(),
        }
    }

    /// Appends a delta to the set of inflight account updates.
    pub fn add_delta(&mut self, delta: &AccountDelta) {
        let mut state = self.latest_account();
        state
            .apply_delta(delta)
            .expect("network account delta should apply since it was accepted by the mempool");

        self.inflight.push_back(state);
    }

    /// Commits the oldest account state delta.
    ///
    /// # Panics
    ///
    /// Panics if there are no deltas to commit.
    pub fn commit_delta(&mut self) {
        self.committed = self.inflight.pop_front().expect("must have a delta to commit").into();
    }

    /// Reverts the newest account state delta.
    ///
    /// # Returns
    ///
    /// Returns `true` if this reverted the account creation delta. The caller _must_ remove this
    /// account and associated notes as calls to `account` will panic.
    ///
    /// # Panics
    ///
    /// Panics if there are no deltas to revert.
    #[must_use = "must remove this account and its notes"]
    pub fn revert_delta(&mut self) -> bool {
        self.inflight.pop_back().expect("must have a delta to revert");
        self.committed.is_none() && self.inflight.is_empty()
    }

    /// Adds a new network note making it available for consumption.
    pub fn add_note(&mut self, note: NetworkNote) {
        self.available_notes.insert(note.nullifier(), InflightNetworkNote::new(note));
    }

    /// Removes the note completely.
    pub fn revert_note(&mut self, note: Nullifier) {
        // Transactions can be reverted out of order.
        //
        // This means the tx which nullified the note might not have been reverted yet, and the note
        // might still be in the nullified
        self.available_notes.remove(&note);
        self.nullified_notes.remove(&note);
    }

    /// Marks a note as being consumed.
    ///
    /// The note data is retained until the nullifier is committed.
    ///
    /// # Panics
    ///
    /// Panics if the note does not exist or was already nullified.
    pub fn add_nullifier(&mut self, nullifier: Nullifier) {
        let note = self
            .available_notes
            .remove(&nullifier)
            .expect("note must be available to nullify");

        self.nullified_notes.insert(nullifier, note.into_inner());
    }

    /// Marks a nullifier as being committed, removing the associated note data entirely.
    ///
    /// # Panics
    ///
    /// Panics if the associated note is not marked as nullified.
    pub fn commit_nullifier(&mut self, nullifier: Nullifier) {
        self.nullified_notes
            .remove(&nullifier)
            .expect("committed nullified note should be in the nullified set");
    }

    /// Reverts a nullifier, marking the associated note as available again.
    pub fn revert_nullifier(&mut self, nullifier: Nullifier) {
        // Transactions can be reverted out of order.
        //
        // The note may already have been fully removed by `revert_note` if the transaction creating
        // the note was reverted before the transaction that consumed it.
        if let Some(note) = self.nullified_notes.remove(&nullifier) {
            self.available_notes.insert(nullifier, InflightNetworkNote::new(note));
        }
    }

    pub fn notes(&self) -> impl Iterator<Item = &InflightNetworkNote> {
        self.available_notes.values()
    }

    pub fn drain_failing_notes(&mut self, max_attempts: usize) {
        self.available_notes.retain(|_, note| note.attempt_count() <= max_attempts);
    }

    /// Returns the latest inflight account state.
    pub fn latest_account(&self) -> Account {
        self.inflight
            .back()
            .or(self.committed.as_ref())
            .expect("account must have either a committed or inflight state")
            .clone()
    }

    /// Returns `true` if there is no inflight state being tracked.
    ///
    /// This implies this state is safe to remove without losing uncommitted data.
    pub fn is_empty(&self) -> bool {
        self.inflight.is_empty()
            && self.available_notes.is_empty()
            && self.nullified_notes.is_empty()
    }

    /// Marks the specified notes as failed.
    pub fn fail(&mut self, notes: &[InflightNetworkNote], block_num: BlockNumber) {
        for nullifier in notes.iter().map(|note| note.to_inner().nullifier()) {
            if let Some(note) = self.available_notes.get_mut(&nullifier) {
                note.fail(block_num);
            } else {
                tracing::warn!("failed to find note with nullifier {nullifier}");
            }
        }
    }
}

// NETWORK ACCOUNT UPDATE
// ================================================================================================

#[derive(Clone)]
pub enum NetworkAccountUpdate {
    New(Account),
    Delta(AccountDelta),
}

impl NetworkAccountUpdate {
    pub fn from_protocol(update: AccountUpdateDetails) -> Option<Self> {
        let update = match update {
            AccountUpdateDetails::Private => return None,
            AccountUpdateDetails::New(update) => Self::New(update),
            AccountUpdateDetails::Delta(update) => Self::Delta(update),
        };

        update.account_id().is_network().then_some(update)
    }

    pub fn prefix(&self) -> NetworkAccountPrefix {
        // SAFETY: This is a network account by construction.
        self.account_id().try_into().unwrap()
    }

    fn account_id(&self) -> AccountId {
        match self {
            NetworkAccountUpdate::New(account) => account.id(),
            NetworkAccountUpdate::Delta(account_delta) => account_delta.id(),
        }
    }
}
