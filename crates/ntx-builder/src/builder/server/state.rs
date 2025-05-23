use std::collections::{BTreeMap, BTreeSet, VecDeque};

use miden_objects::{
    note::{Note, NoteId, NoteTag, Nullifier},
    transaction::TransactionId,
};

/// Contains state of the network transaction builder.
///
/// Notes are mainly kept in a [`VecDeque`], but also indexed by nullifiers and note tags in order
/// to enable simple lookups when discarding by nullifiers and deciding which notes to execute
/// against a unique account, respectively.
/// Additionally, a list of inflight notes is kept to track their lifecycle.
#[derive(Debug)]
pub struct NtxBuilderState {
    /// Contains a queue that provides ordering of accounts to execute against
    account_queue: VecDeque<NoteTag>,
    /// A map of nullifiers mapped to their note IDs
    notes_by_nullifiers: BTreeMap<Nullifier, NoteId>,
    /// Pending network notes that have not been consumed as part of a committed transaction.
    pending_notes_by_tag: BTreeMap<NoteTag, Vec<Note>>,
    /// Inflight network notes with their associated transaction IDs
    inflight_transactions: BTreeMap<TransactionId, Vec<Note>>,
}

impl NtxBuilderState {
    pub fn new(unconsumed_network_notes: Vec<Note>) -> Self {
        let mut state = Self {
            account_queue: VecDeque::new(),
            notes_by_nullifiers: BTreeMap::new(),
            pending_notes_by_tag: BTreeMap::new(),
            inflight_transactions: BTreeMap::new(),
        };
        state.add_unconsumed_notes(unconsumed_network_notes);
        state
    }

    /// Check if there are any pending notes
    pub fn has_unconsumed_notes(&self) -> bool {
        !self.account_queue.is_empty()
    }

    /// Add network notes to the pending notes queue.
    /// Also adds nullifier |-> note ID mapping, and adds it to the set of notes by tag.
    pub fn add_unconsumed_notes(&mut self, notes: Vec<Note>) {
        for note in notes {
            self.notes_by_nullifiers.insert(note.nullifier(), note.id());

            let tag = note.metadata().tag();
            self.pending_notes_by_tag.entry(tag).or_default().push(note.clone());

            self.account_queue.push_back(note.metadata().tag());
        }
    }

    /// Discard a transaction, moving its notes back to pending
    /// Returns the number of notes moved back to pending status
    pub fn discard_transaction(&mut self, tx_id: TransactionId) -> usize {
        if let Some(notes) = self.inflight_transactions.remove(&tx_id) {
            let n = notes.len();
            self.account_queue.push_back(notes.first().unwrap().metadata().tag());
            for note in notes {
                let tag = note.metadata().tag();
                self.pending_notes_by_tag.entry(tag).or_default().push(note);
            }
            // SAFETY: All transactions contain at least a note
            n
        } else {
            0
        }
    }

    /// Mark a transaction as committed, removing its notes from the inflight notes set.
    /// Returns the number of notes that were committed
    pub fn commit_transaction(&mut self, tx_id: TransactionId) -> usize {
        if let Some(notes) = self.inflight_transactions.remove(&tx_id) {
            notes.len()
        } else {
            0
        }
    }

    /// Returns the tag of the next note scheduled in the global queue
    pub fn get_next_note_tag(&self) -> Option<NoteTag> {
        self.account_queue.front().copied()
    }

    /// Discard every note whose nullifier is in the input slice.
    pub fn discard_by_nullifiers(&mut self, nullifiers: &[Nullifier]) {
        let mut to_remove: BTreeSet<NoteId> = BTreeSet::new();
        for n in nullifiers {
            if let Some(id) = self.notes_by_nullifiers.remove(n) {
                to_remove.insert(id);
            }
        }

        if to_remove.is_empty() {
            return;
        }

        self.pending_notes_by_tag.retain(|_, q| {
            q.retain(|note| !to_remove.contains(&note.id()));
            !q.is_empty()
        });

        self.inflight_transactions.retain(|_, notes| {
            notes.retain(|note| !to_remove.contains(&note.id()));
            !notes.is_empty()
        });
    }
}

// TODO: Add tests
