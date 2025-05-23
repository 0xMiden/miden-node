use std::collections::{BTreeMap, BTreeSet, VecDeque};

use miden_node_utils::note_tag::NetworkNote;
use miden_objects::{
    note::{Note, NoteId, NoteTag, Nullifier},
    transaction::TransactionId,
};

/// Max number of notes taken for executing a single network transaction
const MAX_BATCH: usize = 50;

/// Maintains state of the network notes for the transaction builder.
///
/// Notes are mainly kept in a [`VecDeque`], but also indexed by nullifiers and note tags in order
/// to enable simple lookups when discarding by nullifiers and deciding which notes to execute
/// against a unique account, respectively.
/// Additionally, a list of inflight notes is kept to track their lifecycle.
#[derive(Debug)]
pub struct PendingNotes {
    /// Contains a queue that provides ordering of accounts to execute against
    account_queue: VecDeque<NoteTag>,
    /// Pending network notes that have not been consumed as part of a committed transaction.
    by_tag: BTreeMap<NoteTag, Vec<NetworkNote>>,
    /// A map of nullifiers mapped to their note IDs
    by_nullifier: BTreeMap<Nullifier, NoteId>,
    /// Inflight network notes with their associated transaction IDs
    inflight_txs: BTreeMap<TransactionId, Vec<NetworkNote>>,
}

impl PendingNotes {
    pub fn new(unconsumed_network_notes: Vec<NetworkNote>) -> Self {
        let mut state = Self {
            account_queue: VecDeque::new(),
            by_tag: BTreeMap::new(),
            by_nullifier: BTreeMap::new(),
            inflight_txs: BTreeMap::new(),
        };
        state.queue_unconsumed_notes(unconsumed_network_notes);
        state
    }

    /// Add network notes to the pending notes queue.
    pub fn queue_unconsumed_notes(&mut self, notes: impl IntoIterator<Item = NetworkNote>) {
        for n in notes {
            let tag = n.metadata().tag();

            self.by_nullifier.insert(n.nullifier(), n.id());
            self.by_tag.entry(tag).or_default().push(n.clone());

            self.account_queue.push_back(tag);
        }
    }

    /// Returns the next set of notes with the next scheduled tag in the global queue
    /// (up to `MAX_BATCH`)
    pub fn take_next_notes_by_tag(&mut self) -> Option<(NoteTag, Vec<NetworkNote>)> {
        let next_tag = *self.account_queue.front()?;

        let bucket = self.by_tag.get_mut(&next_tag)?;
        let note_count = bucket.len().min(MAX_BATCH);
        let batch: Vec<NetworkNote> = bucket.drain(..note_count).collect();

        if bucket.is_empty() {
            self.by_tag.remove(&next_tag);
        }

        Some((next_tag, batch))
    }

    /// Move the input notes into inflight status under the given transaction ID
    pub fn mark_inflight(&mut self, tx_id: TransactionId, notes: Vec<NetworkNote>) {
        if !notes.is_empty() {
            self.inflight_txs.entry(tx_id).or_default().extend(notes);
        }
    }

    /// Discard every note whose nullifier is in the input set.
    pub fn discard_by_nullifiers(&mut self, nullifiers: &[Nullifier]) {
        let mut ids = BTreeSet::new();
        for nf in nullifiers {
            if let Some(id) = self.by_nullifier.remove(nf) {
                ids.insert(id);
            }
        }
        if ids.is_empty() {
            return;
        }

        self.by_tag.retain(|_, bucket| {
            bucket.retain(|n| !ids.contains(&n.id()));
            !bucket.is_empty()
        });

        self.inflight_txs.retain(|_, notes| {
            notes.retain(|n| !ids.contains(&n.id()));
            !notes.is_empty()
        });
    }

    /// Mark a transaction as committed, removing its notes from the inflight notes set.
    /// Returns the number of notes that were committed.
    pub fn commit_inflight(&mut self, tx_id: TransactionId) -> usize {
        if let Some(notes) = self.inflight_txs.remove(&tx_id) {
            for n in &notes {
                self.by_nullifier.remove(&n.nullifier());
            }
            notes.len()
        } else {
            0
        }
    }

    pub fn rollback_inflight(&mut self, tx_id: TransactionId) -> usize {
        if let Some(notes) = self.inflight_txs.remove(&tx_id) {
            let count = notes.len();
            // SAFETY: All transactions contain at least a note
            self.account_queue.push_back(notes.first().unwrap().metadata().tag());
            for n in notes {
                self.by_tag.entry(n.metadata().tag()).or_default().push(n.clone());
                self.by_nullifier.insert(n.nullifier(), n.id());
            }

            count
        } else {
            0
        }
    }
}

// TODO: tests
