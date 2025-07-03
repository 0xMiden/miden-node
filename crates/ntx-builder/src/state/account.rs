use std::collections::{BTreeMap, VecDeque};

use miden_node_proto::domain::{account::NetworkAccountPrefix, note::NetworkNote};
use miden_objects::{
    account::{Account, AccountDelta, AccountId, delta::AccountUpdateDetails},
    note::Nullifier,
};

/// Tracks the state of a network account and its notes.
#[derive(Default)]
pub struct AccountState {
    /// Inflight account updates in chronological order.
    deltas: VecDeque<NetworkAccountUpdate>,

    /// Unconsumed notes of this account.
    available_notes: BTreeMap<Nullifier, NetworkNote>,

    /// Notes which have been consumed by transactions that are still inflight.
    nullified_notes: BTreeMap<Nullifier, NetworkNote>,
}

impl AccountState {
    /// Appends a delta to the set of inflight account updates.
    pub fn add_delta(&mut self, delta: NetworkAccountUpdate) {
        self.deltas.push_back(delta);
    }

    /// Commits the oldest account state delta.
    ///
    /// # Panics
    ///
    /// Panics if there are no deltas to commit.
    pub fn commit_delta(&mut self) {
        self.deltas.pop_front().expect("must have a delta to commit");
    }

    /// Reverts the newest account state delta.
    ///
    /// # Panics
    ///
    /// Panics if there are no deltas to revert.
    pub fn revert_delta(&mut self) {
        self.deltas.pop_back().expect("must have a delta to revert");
    }

    /// Adds a new network note making it available for consumption.
    pub fn add_note(&mut self, note: NetworkNote) {
        self.available_notes.insert(note.nullifier(), note);
    }

    /// Removes the note completely.
    pub fn revert_note(&mut self, note: Nullifier) {
        // Transactions can be reverted out of order.
        //
        // This means the tx which nullified the note might not have been reverted yet, and the note might still be in the nullified
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

        self.nullified_notes.insert(nullifier, note);
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
        // The note may already have been fully removed by `revert_note` if the transaction creating the note was reverted before the transaction that consumed it.
        if let Some(note) = self.nullified_notes.remove(&nullifier) {
            self.available_notes.insert(nullifier, note);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.deltas.is_empty() && self.available_notes.is_empty() && self.nullified_notes.is_empty()
    }

    pub fn has_notes_available(&self) -> bool {
        !self.available_notes.is_empty()
    }
}

pub enum Status {
    NoteAvailable,
    NoNotesAvailable,
    Empty,
}

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
