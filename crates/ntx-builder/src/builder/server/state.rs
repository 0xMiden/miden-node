use std::collections::{BTreeMap, BTreeSet, VecDeque};

use miden_objects::{
    note::{Note, NoteId, NoteTag, Nullifier},
    transaction::TransactionId,
};

const MAX_BATCH: usize = 50;

/// Maintains state of the network notes for the transaction builder.
///
/// Notes are mainly kept in a [`VecDeque`], but also indexed by nullifiers and note tags in order
/// to enable simple lookups when discarding by nullifiers and deciding which notes to execute
/// against a unique account, respectively.
/// Additionally, a list of inflight notes is kept to track their lifecycle.
#[derive(Debug)]
pub struct PendingNotes {
    queue: VecDeque<Note>,
    by_tag: BTreeMap<NoteTag, Vec<Note>>,
    by_nullifier: BTreeMap<Nullifier, NoteId>,
    inflight: BTreeMap<TransactionId, Vec<Note>>,
}

impl PendingNotes {
    pub fn new(unconsumed_network_notes: Vec<Note>) -> Self {
        let mut state = Self {
            queue: VecDeque::new(),
            by_tag: BTreeMap::new(),
            by_nullifier: BTreeMap::new(),
            inflight: BTreeMap::new(),
        };
        state.queue_unconsumed_notes(unconsumed_network_notes);
        state
    }

    /// Add network notes to the pending notes queue.
    pub fn queue_unconsumed_notes(&mut self, notes: Vec<Note>) {
        for n in notes {
            self.queue.push_back(n.clone());
            self.by_tag.entry(n.metadata().tag()).or_default().push(n.clone());
            self.by_nullifier.insert(n.nullifier(), n.id());
        }
    }

    /// Returns the next set of notes with the next scheduled tag in the global queue
    /// (up to `MAX_BATCH`)
    pub fn take_next_notes_by_tag(&mut self) -> Option<(NoteTag, Vec<Note>)> {
        let next_tag = self.queue.front()?.metadata().tag();

        let bucket = self.by_tag.get_mut(&next_tag)?;
        let note_count = bucket.len().min(MAX_BATCH);
        let batch: Vec<Note> = bucket.drain(..note_count).collect();

        if bucket.is_empty() {
            self.by_tag.remove(&next_tag);
        }

        let ids: BTreeSet<NoteId> = batch.iter().map(Note::id).collect();
        self.queue.retain(|n| !ids.contains(&n.id()));

        Some((next_tag, batch))
    }

    /// Move the input notes into inflight status under the given transaction ID
    pub fn mark_inflight(&mut self, tx_id: TransactionId, notes: Vec<Note>) {
        if !notes.is_empty() {
            self.inflight.entry(tx_id).or_default().extend(notes);
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

        self.queue.retain(|n| !ids.contains(&n.id()));

        self.inflight.retain(|_, notes| {
            notes.retain(|n| !ids.contains(&n.id()));
            !notes.is_empty()
        });
    }

    /// Mark a transaction as committed, removing its notes from the inflight notes set.
    /// Returns the number of notes that were committed.
    pub fn commit_inflight(&mut self, tx_id: TransactionId) -> usize {
        if let Some(notes) = self.inflight.remove(&tx_id) {
            for n in &notes {
                self.by_nullifier.remove(&n.nullifier());
            }
            notes.len()
        } else {
            0
        }
    }

    pub fn rollback_inflight(&mut self, tx_id: TransactionId) -> usize {
        if let Some(notes) = self.inflight.remove(&tx_id) {
            let count = notes.len();

            for n in notes {
                self.queue.push_back(n.clone());
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
