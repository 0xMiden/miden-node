use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use miden_node_proto::domain::{account::NetworkAccountPrefix, note::NetworkNote};
use miden_objects::{note::Nullifier, transaction::TransactionId};

/// Manages the available [`NetworkNotes`](NetworkNote) and the inflight state that pertains to
/// them.
///
/// It allows selecting a network account with notes available to consume.
///
/// It tracks inflight transaction's that create or consume network notes, and removes the note data
/// if these are committed or reverted.
#[derive(Default)]
pub struct Notes {
    /// Viable candidate accounts available for selection.
    ///
    /// They are guaranteed to have network notes.
    queue: VecDeque<NetworkAccountPrefix>,

    /// Accounts that have been selected and not yet deselected.
    in_progress: HashSet<NetworkAccountPrefix>,

    /// Notes available for each account. This _excludes_ the notes in `nullified`.
    ///
    /// We use [`Nullifier`] as the note ID as it simplifies our internal tracking
    /// and is equivalently unique.
    by_account: HashMap<NetworkAccountPrefix, BTreeSet<Nullifier>>,

    /// Notes that have not been consumed and are available.
    ///
    /// We use [`Nullifier`] as the note ID as it simplifies our internal tracking
    /// and is equivalently unique.
    available: BTreeMap<Nullifier, NetworkNote>,

    /// Notes that have been consumed and are unavailable.
    ///
    /// These are tracked until the nullifier's transaction is committed.
    ///
    /// We use [`Nullifier`] as the note ID as it simplifies our internal tracking
    /// and is equivalently unique.
    nullified: BTreeMap<Nullifier, NetworkNote>,

    /// Transactions that are inflight in the mempool and their associated state impact.
    txs: BTreeMap<TransactionId, InflightTx>,
}

impl Notes {
    /// Creates a new [`Notes`] instance from the given set of already committed network notes.
    pub fn new(committed: impl Iterator<Item = NetworkNote>) -> Self {
        let mut notes = Self::default();

        for note in committed {
            notes.insert_note(note);
        }

        notes
    }

    /// Adds the transaction to the state, making the created notes available for selection and
    /// nullifying the consumed notes.
    pub fn add(&mut self, tx: TransactionId, created: Vec<NetworkNote>, consumed: Vec<Nullifier>) {
        let created_nul = created.iter().map(NetworkNote::nullifier).collect();
        for note in created {
            self.insert_note(note);
        }

        // Not all nullifiers are network note nullifiers.
        let mut actually_consumed = Vec::with_capacity(consumed.len());
        for nullifier in consumed {
            if self.nullify_note(nullifier) {
                actually_consumed.push(nullifier);
            }
        }

        self.txs.insert(
            tx,
            InflightTx {
                created: created_nul,
                consumed: actually_consumed,
            },
        );
    }

    /// Commits the state associated with this transaction.
    ///
    /// More specifically, this removes all notes that were
    /// nullified by this transaction.
    ///
    /// Notes created by this transaction must remain in order to
    /// still be consumable by other transactions.
    pub fn commit(&mut self, tx: &TransactionId) {
        let Some(tx) = self.txs.remove(tx) else {
            return;
        };

        // Since the tx is being committed the notes must already be marked as nullified,
        // and therefore only exist in the nullified set.
        for nullifier in tx.consumed {
            assert!(
                self.nullified.remove(&nullifier).is_some(),
                "notes nullified by a committed transaction must be in the nullified set"
            );
        }
    }

    /// Reverts the state changes associated with this transaction.
    ///
    /// More specifically, this removes all notes that were
    /// created by this transaction, and moves all nullified notes back
    /// into the available pool.
    pub fn revert(&mut self, tx: &TransactionId) {
        let Some(tx) = self.txs.remove(tx) else {
            return;
        };

        for note in tx.created {
            self.remove_available_note(&note);
            // We can't guarantee the order that reverted tx's are submitted here.
            //
            // To be safe, we also remove the tx from nullified. This covers the case where
            // the tx consuming the note is reverted _after_ the tx that created it.
            self.nullified.remove(&note);
        }

        for note in tx.consumed {
            // Its possible that the note was already removed by a prior reverting tx.
            //
            // (see above).
            let Some(note) = self.nullified.remove(&note) else {
                continue;
            };

            self.insert_note(note);
        }
    }

    /// Returns the next candidate for a network transaction.
    ///
    /// The returned account is guaranteed to have network notes available.
    ///
    /// Note that this account is internally marked as in-progress and cannot be
    /// selected again until it has been deselected.
    pub fn select(&mut self) -> Option<NetworkAccountPrefix> {
        let account = self.queue.pop_front()?;
        self.in_progress.insert(account);

        Some(account)
    }

    /// Marks an account as no longer in-progress.
    ///
    /// This makes the account available for selection again.
    ///
    /// This should be called if a candidate transaction was cancelled, failed or
    /// the account was updated via a mempool event.
    ///
    /// This should _not_ be called if the transaction completes locally. Instead it
    /// should be called directly _after_ adding the transaction's mempool event.
    ///
    /// This is required so that the internal state accurately reflects the transaction's
    /// note state changes.
    pub fn deselect(&mut self, account: NetworkAccountPrefix) {
        if !self.in_progress.remove(&account) {
            tracing::warn!(?account, "deselected an account that was not in progress");
            return;
        }

        self.queue.push_back(account);
    }

    /// Returns an iterator over all notes available for the given network account.
    ///
    /// This iterator can be empty if the account has no notes.
    pub fn get(&mut self, account: NetworkAccountPrefix) -> impl Iterator<Item = &NetworkNote> {
        self.by_account
            .get(&account)
            .map(BTreeSet::iter)
            .unwrap_or_default()
            .filter_map(|note| self.available.get(note))
    }

    /// Inserts a new note, making it available for selection.
    fn insert_note(&mut self, note: NetworkNote) {
        use std::collections::hash_map::Entry;

        let account = note.account_prefix();

        // Accounts with no entry also need to be added to the queue
        // so they are available for selection.
        match self.by_account.entry(account) {
            Entry::Occupied(occupied) => occupied,
            Entry::Vacant(vacant) => {
                self.queue.push_back(account);
                vacant.insert_entry(BTreeSet::default())
            },
        }
        .get_mut()
        .insert(note.nullifier());

        self.available.insert(note.nullifier(), note);
    }

    /// Moves a note from the available set and into the nullified set.
    fn nullify_note(&mut self, nullifier: Nullifier) -> bool {
        assert!(!self.nullified.contains_key(&nullifier), "note was already nullified");

        let Some(note) = self.remove_available_note(&nullifier) else {
            return false;
        };
        self.nullified.insert(nullifier, note);
        true
    }

    /// Removes the note from the available set.
    ///
    /// If this is the last available note for a given account, then this is also removed from
    /// tracking.
    fn remove_available_note(&mut self, nullifier: &Nullifier) -> Option<NetworkNote> {
        let note = self.available.remove(nullifier)?;

        // Update internal tracking.
        let account = note.account_prefix();
        let by_account = self
            .by_account
            .get_mut(&account)
            .expect("account must be tracked for an available note");
        by_account.remove(nullifier);

        // Remove account from tracking if its empty.
        if by_account.is_empty() {
            self.by_account.remove(&account);
            if let Some(queued) = self.queue.iter().position(|q| *q == account) {
                self.queue.remove(queued);
            }
        }

        Some(note)
    }
}

/// Describes a transaction's impact on the note state.
struct InflightTx {
    /// Notes that this transaction created.
    ///
    /// Nullifiers are used to simplify tracking in the larger state struct.
    created: Vec<Nullifier>,
    /// Notes that this transaction consumed aka true nullifier list.
    consumed: Vec<Nullifier>,
}
