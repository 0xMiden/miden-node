use std::collections::{BTreeMap, VecDeque};

use miden_node_proto::domain::note::NetworkNote;
use miden_objects::{
    note::{NoteId, NoteTag, Nullifier},
    transaction::TransactionId,
};

/// Max number of notes taken for executing a single network transaction
const MAX_BATCH: usize = 50;

/// Maintains the pool of network notes the transaction builder still has to process.
///
/// A queue of note tags is maintained to identify which account executes a transaction with a
/// set of notes next. Besides this, notes are also indexed by tag, and by nullifier.
///
/// Notes locked inside an unconfirmed transaction get moved to `inflight_txs` and can get rolled
/// back into the queue or get removed when that transaction is either rolled back or committed,
/// respectively.
///
/// A note lives in either `by_tag` or `inflight_txs` (but not both), and a tag stays in
/// `account_queue` only while `by_tag` still holds notes for it.
#[derive(Debug)]
pub struct PendingNotes {
    /// Contains the current set of unconsumed notes indexed by ID.
    note_by_id: BTreeMap<NoteId, NetworkNote>,
    /// Contains a queue that provides ordering of accounts to execute against.
    account_queue: VecDeque<NoteTag>,
    /// Pending network notes that have not been consumed as part of a committed transaction.
    by_tag: BTreeMap<NoteTag, Vec<NoteId>>,
    /// A map of nullifiers mapped to their note IDs.
    by_nullifier: BTreeMap<Nullifier, NoteId>,
    /// Inflight network notes with their associated transaction IDs.
    inflight_txs: BTreeMap<TransactionId, Vec<NoteId>>,
}

impl PendingNotes {
    pub fn new(unconsumed_network_notes: Vec<NetworkNote>) -> Self {
        let mut state = Self {
            account_queue: VecDeque::new(),
            by_tag: BTreeMap::new(),
            by_nullifier: BTreeMap::new(),
            inflight_txs: BTreeMap::new(),
            note_by_id: BTreeMap::new(),
        };
        state.queue_unconsumed_notes(unconsumed_network_notes);
        state
    }

    /// Add network notes to the pending notes queue.
    pub fn queue_unconsumed_notes(&mut self, notes: impl IntoIterator<Item = NetworkNote>) {
        for note in notes {
            let tag = note.metadata().tag();
            let id = note.id();

            self.by_nullifier.insert(note.nullifier(), id);
            self.by_tag.entry(tag).or_default().push(id);
            self.note_by_id.insert(id, note);

            self.push_tag_once(tag);
        }
    }

    /// Returns the next set of notes with the next scheduled tag in the global queue
    /// (up to `MAX_BATCH`)
    pub fn take_next_notes_by_tag(&mut self) -> Option<(NoteTag, Vec<NetworkNote>)> {
        let next_tag = *self.account_queue.front()?;

        let bucket = self.by_tag.get_mut(&next_tag)?;
        let take = bucket.len().min(MAX_BATCH);
        let ids: Vec<NoteId> = bucket.drain(..take).collect();

        self.prune_if_empty(next_tag);

        let notes: Vec<NetworkNote> =
            ids.iter().filter_map(|id| self.note_by_id.get(id).cloned()).collect();

        Some((next_tag, notes))
    }

    /// Move the notes whose nullifiers belong to the input `nullifiers` into the in-flight
    /// set for the given `tx_id`, removing them from the indices and pruning empty tag buckets and
    /// queue entries.
    pub fn insert_inflight(
        &mut self,
        tx_id: TransactionId,
        nullifiers: impl IntoIterator<Item = Nullifier>,
    ) {
        let mut nullifiers = nullifiers.into_iter().peekable();
        if nullifiers.peek().is_none() {
            return;
        }
        let mut moved = Vec::new();

        for nullifier in nullifiers {
            if let Some(id) = self.by_nullifier.remove(&nullifier) {
                moved.push(id);

                if let Some(note) = self.note_by_id.get(&id) {
                    let tag = note.metadata().tag();

                    if let Some(bucket) = self.by_tag.get_mut(&tag) {
                        bucket.retain(|&x| x != id);
                        self.prune_if_empty(tag);
                    }
                }
            }
        }

        self.inflight_txs.entry(tx_id).or_default().extend(moved);
    }

    /// Mark a transaction as committed, removing its notes from the inflight notes set.
    /// Returns the number of notes that were committed.
    pub fn commit_inflight(&mut self, tx_id: TransactionId) -> usize {
        if let Some(ids) = self.inflight_txs.remove(&tx_id) {
            for id in &ids {
                if let Some(n) = self.note_by_id.remove(id) {
                    self.by_nullifier.remove(&n.nullifier());
                }
            }
            ids.len()
        } else {
            0
        }
    }

    /// Rolls back a failed transaction: restore its notes to the pending pools, re-queue the tag,
    /// and return the number of notes restored.
    pub fn rollback_inflight(&mut self, tx_id: TransactionId) -> usize {
        if let Some(ids) = self.inflight_txs.remove(&tx_id) {
            let count = ids.len();
            for id in ids {
                if let Some(note) = self.note_by_id.get(&id) {
                    let tag = note.metadata().tag();
                    self.by_nullifier.insert(note.nullifier(), id);
                    self.by_tag.entry(tag).or_default().push(id);

                    self.push_tag_once(note.metadata().tag());
                }
            }
            count
        } else {
            0
        }
    }

    /// Enqueue `tag`to the queue only if it isn’t already present
    fn push_tag_once(&mut self, tag: NoteTag) {
        if !self.account_queue.contains(&tag) {
            self.account_queue.push_back(tag);
        }
    }

    fn prune_if_empty(&mut self, tag: NoteTag) {
        if matches!(self.by_tag.get(&tag), Some(bucket) if bucket.is_empty()) {
            self.by_tag.remove(&tag);
        }
        if !self.by_tag.contains_key(&tag) {
            self.account_queue.retain(|t| t != &tag);
        }
    }
}

#[cfg(test)]
mod tests {
    use miden_objects::{
        Felt,
        crypto::rand::{FeltRng, RpoRandomCoin},
        note::{
            Note, NoteAssets, NoteExecutionMode, NoteInputs, NoteMetadata, NoteRecipient,
            NoteScript,
        },
        testing::account_id::ACCOUNT_ID_PRIVATE_SENDER,
    };
    use rand::{Rng, rng};

    use super::*;

    fn mock_note(tag_suffix: u16) -> NetworkNote {
        let metadata = NoteMetadata::new(
            ACCOUNT_ID_PRIVATE_SENDER.try_into().unwrap(),
            miden_objects::note::NoteType::Public,
            NoteTag::for_public_use_case(0, tag_suffix, NoteExecutionMode::Network).unwrap(),
            miden_objects::note::NoteExecutionHint::Always,
            Felt::new(0),
        )
        .unwrap();
        let note_assets = NoteAssets::new(Vec::default()).unwrap();
        let mut thread_rng = rng();
        let coin_seed: [u64; 4] = thread_rng.random();

        let mut rng = RpoRandomCoin::new(coin_seed.map(Felt::new));

        let note_recipient =
            NoteRecipient::new(rng.draw_word(), NoteScript::mock(), NoteInputs::default());

        Note::new(note_assets, metadata, note_recipient).try_into().unwrap()
    }

    fn mock_tx_id() -> TransactionId {
        let mut thread_rng = rng();
        let coin_seed: [u64; 4] = thread_rng.random();

        let mut rng = RpoRandomCoin::new(coin_seed.map(Felt::new));

        rng.draw_word().into()
    }

    #[test]
    fn notes_enqueue_once() {
        let notes = vec![mock_note(1), mock_note(1), mock_note(2)];
        let pending = PendingNotes::new(notes.clone());

        for n in &notes {
            assert!(pending.note_by_id.contains_key(&n.id()));
            assert_eq!(pending.by_nullifier.get(&n.nullifier()), Some(&n.id()));
        }
        assert_eq!(pending.by_tag.get(&notes[0].metadata().tag()).unwrap().len(), 2);

        // only 2 tags remain (1 and 2)
        let tags: Vec<_> = pending.account_queue.iter().copied().collect();
        assert_eq!(tags, vec![notes[0].metadata().tag(), notes[2].metadata().tag()]);
    }

    #[test]
    fn insert_inflight_moves_notes_correctly() {
        let a = mock_note(1);
        let b = mock_note(1);
        let c = mock_note(2);
        let mut pending = PendingNotes::new(vec![a.clone(), b.clone(), c.clone()]);

        let tx = mock_tx_id();
        pending.insert_inflight(tx, [a.nullifier(), c.nullifier()]);

        assert!(!pending.by_nullifier.contains_key(&a.nullifier()));
        assert!(!pending.by_nullifier.contains_key(&c.nullifier()));
        assert!(!pending.by_tag.values().any(|bucket| bucket.contains(&a.id())));
        assert!(!pending.by_tag.values().any(|bucket| bucket.contains(&c.id())));

        assert_eq!(pending.inflight_txs.get(&tx).unwrap().len(), 2);
        assert!(pending.note_by_id.contains_key(&a.id()));

        assert!(pending.by_tag.get(&b.metadata().tag()).unwrap().contains(&b.id()));

        assert_eq!(pending.account_queue.iter().filter(|&&t| t == b.metadata().tag()).count(), 1);
    }

    #[test]
    fn commit_inflight_drops_everything() {
        let a = mock_note(1);
        let mut pending = PendingNotes::new(vec![a.clone()]);

        let tx = mock_tx_id();
        pending.insert_inflight(tx, [a.nullifier()]);

        let n = pending.commit_inflight(tx);
        assert_eq!(n, 1);

        assert!(!pending.note_by_id.contains_key(&a.id()));
        assert!(!pending.by_nullifier.contains_key(&a.nullifier()));
        assert!(pending.inflight_txs.is_empty());
    }

    #[test]
    fn rollback_tx_restores_state() {
        let a = mock_note(1);
        let mut pending = PendingNotes::new(vec![a.clone()]);

        let tx = mock_tx_id();
        pending.insert_inflight(tx, [a.nullifier()]);

        let n = pending.rollback_inflight(tx);
        assert_eq!(n, 1);

        assert!(pending.by_nullifier.contains_key(&a.nullifier()));
        assert!(pending.by_tag.get(&a.metadata().tag()).unwrap().contains(&a.id()));

        assert_eq!(pending.account_queue.iter().filter(|&&t| t == a.metadata().tag()).count(), 1);

        assert!(!pending.inflight_txs.contains_key(&tx));
    }
}
