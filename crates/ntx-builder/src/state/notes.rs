use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use miden_node_proto::domain::{account::NetworkAccountPrefix, note::NetworkNote};
use miden_objects::{note::Nullifier, transaction::TransactionId};

struct Notes {
    queue: VecDeque<NetworkAccountPrefix>,
    in_progress: HashSet<NetworkAccountPrefix>,
    by_account: HashMap<NetworkAccountPrefix, BTreeSet<Nullifier>>,

    available: BTreeMap<Nullifier, NetworkNote>,
    nullified: BTreeMap<Nullifier, NetworkNote>,

    txs: BTreeMap<TransactionId, InflightTx>,
}

impl Notes {
    pub fn add(&mut self, tx: TransactionId, created: Vec<NetworkNote>, consumed: Vec<Nullifier>) {
        // TODO: rest of the owl.
        self.txs.insert(
            tx,
            InflightTx {
                created: created.iter().map(NetworkNote::nullifier).collect(),
                consumed: consumed.clone(),
            },
        );

        for note in created {
            self.insert_note(note);
        }

        for nullifier in consumed {
            self.consume_note(nullifier);
        }
    }

    pub fn commit(&mut self, tx: &TransactionId) {
        let Some(tx) = self.txs.remove(tx) else {
            return;
        };

        for nullifier in tx.consumed {
            self.nullified.remove(&nullifier);
        }
    }

    pub fn revert(&mut self, tx: &TransactionId) {
        let Some(tx) = self.txs.remove(tx) else {
            return;
        };

        for note in tx.created {
            self.available.remove(&note);
            // We can't guarantee the order that reverted tx's are submitted here,
            // so we also remove the tx from nullified. This covers the case where
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

    pub fn select(&mut self) -> Option<NetworkAccountPrefix> {
        let account = self.queue.pop_front()?;
        self.in_progress.insert(account);

        Some(account)
    }

    pub fn deselect(&mut self, account: NetworkAccountPrefix) {
        if !self.in_progress.remove(&account) {
            tracing::warn!(?account, "deselected an account that was not in progress");
            return;
        }

        self.queue.push_back(account);
    }

    pub fn get(&mut self, account: NetworkAccountPrefix) -> impl Iterator<Item = &NetworkNote> {
        self.by_account
            .get(&account)
            .map(BTreeSet::iter)
            .unwrap_or_default()
            .filter_map(|note| self.available.get(note))
    }

    fn insert_note(&mut self, note: NetworkNote) {
        let account = note.account_prefix();

        // Accounts with no entry also need to be added to the queue
        // so they are available for selection.
        use std::collections::hash_map::Entry;
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

    fn consume_note(&mut self, nullifier: Nullifier) {
        let Some(note) = self.available.remove(&nullifier) else {
            tracing::warn!(%nullifier, "ignoring attempt to consume note that isn't available");
            return;
        };
        let by_account = self
            .by_account
            .get_mut(&note.account_prefix())
            .expect("account must be tracked for an available note");
        by_account.remove(&nullifier);
        if by_account.is_empty() {
            self.by_account.remove(&note.account_prefix());
        }

        self.nullified.insert(nullifier, note);
    }
}

struct InflightTx {
    created: Vec<Nullifier>,
    consumed: Vec<Nullifier>,
}
