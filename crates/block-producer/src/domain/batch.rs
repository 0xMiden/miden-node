use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use miden_objects::Word;
use miden_objects::account::AccountId;
use miden_objects::batch::{BatchAccountUpdate, BatchId, ProvenBatch};
use miden_objects::block::BlockNumber;
use miden_objects::note::{NoteHeader, NoteId, Nullifier};
use miden_objects::transaction::TransactionHeader;

#[derive(Clone, Debug, PartialEq)]
pub struct AuthenticatedBatch {
    inner: Arc<ProvenBatch>,
    /// The account states provided by the store inputs.
    ///
    /// This does not necessarily have to match the batch's initial state as this may still be
    /// modified by inflight transactions.
    store_account_states: HashMap<AccountId, Word>,
    /// Unauthenticated notes that have now been authenticated by the store inputs.
    ///
    /// In other words, notes which were unauthenticated at the time the batch was proven, but
    /// which have since been committed to, and authenticated by the store.
    notes_authenticated_by_store: HashSet<NoteId>,
    /// Chain height that the authentication took place at.
    authentication_height: BlockNumber,
}

impl AuthenticatedBatch {
    pub fn id(&self) -> BatchId {
        self.inner.id()
    }

    pub fn account_updates(&self) -> &BTreeMap<AccountId, BatchAccountUpdate> {
        self.inner.account_updates()
    }

    pub fn store_account_state(&self, account: &AccountId) -> Option<Word> {
        self.store_account_states.get(account).copied()
    }

    pub fn authentication_height(&self) -> BlockNumber {
        self.authentication_height
    }

    pub fn nullifiers(&self) -> impl Iterator<Item = Nullifier> + '_ {
        self.inner.created_nullifiers()
    }

    pub fn output_note_ids(&self) -> impl Iterator<Item = NoteId> + '_ {
        self.inner.output_notes().iter().map(miden_objects::transaction::OutputNote::id)
    }

    /// Notes which were unauthenticated in the transaction __and__ which were not authenticated by
    /// the store inputs.
    pub fn unauthenticated_notes(&self) -> impl Iterator<Item = NoteId> + '_ {
        self.inner
            .input_notes()
            .iter()
            .filter_map(|note| note.header().map(NoteHeader::id))
            .filter(|note_id| !self.notes_authenticated_by_store.contains(note_id))
    }

    pub fn expires_at(&self) -> BlockNumber {
        self.inner.batch_expiration_block_num()
    }

    pub fn transactions(&self) -> &[TransactionHeader] {
        self.inner.transactions().as_slice()
    }
}
