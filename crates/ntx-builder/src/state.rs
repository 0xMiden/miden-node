#![allow(dead_code, reason = "WIP")]
#![allow(clippy::unused_async, reason = "WIP")]

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Not,
    slice::Iter,
};

use account::AccountState;
use miden_node_proto::domain::mempool::MempoolEvent;
use miden_objects::{
    Word,
    account::{Account, AccountId, delta::AccountUpdateDetails},
    block::{BlockHeader, BlockNumber},
    note::Nullifier,
    transaction::{PartialBlockchain, TransactionId},
};
use miden_tx::{DataStore, DataStoreError, MastForestStore};

use crate::{note::NetworkNote, store::StoreClient};

mod account;

pub struct State {
    latest_header: BlockHeader,
    inflight: BTreeMap<TransactionId, InflightTx>,
    accounts: BTreeMap<AccountId, AccountState>,
    notes: BTreeMap<Nullifier, NetworkNote>,
    nullifiers: BTreeSet<Nullifier>,
}

impl State {
    pub fn new(genesis: BlockHeader) -> Self {
        Self {
            latest_header: genesis,
            inflight: BTreeMap::default(),
            accounts: BTreeMap::default(),
            notes: BTreeMap::default(),
            nullifiers: BTreeSet::default(),
        }
    }

    /// Synchronizes the local committed state with that in the store.
    pub async fn sync_committed(&mut self, _store: &StoreClient) -> anyhow::Result<()> {
        todo!()
    }

    pub fn update(&mut self, event: MempoolEvent) {
        match event {
            MempoolEvent::TransactionAdded {
                id,
                nullifiers,
                network_notes,
                account_delta,
            } => {
                self.add_transaction(id, nullifiers, network_notes, account_delta);
            },
            MempoolEvent::BlockCommitted { header, txs } => {
                self.commit_block(header, txs);
            },
            MempoolEvent::TransactionsReverted(txs) => {
                self.revert_transactions(txs);
            },
        }
    }

    fn commit_block(&mut self, header: BlockHeader, txs: Vec<TransactionId>) {
        assert!(header.prev_block_commitment() == self.latest_header.commitment());

        self.latest_header = header;

        for tx in txs {
            let Some(tx) = self.inflight.remove(&tx) else {
                continue;
            };

            if let Some(account_id) = tx.account_delta {
                self.accounts
                    .get_mut(&account_id)
                    .expect("account with delta should be tracked")
                    .commit_one();
            }

            for nullifier in tx.nullifiers {
                self.notes.remove(&nullifier);
                self.nullifiers.remove(&nullifier);
            }
        }
    }

    fn add_transaction(
        &mut self,
        id: TransactionId,
        nullifiers: Vec<Nullifier>,
        network_notes: Vec<miden_objects::note::Note>,
        account_delta: Option<AccountUpdateDetails>,
    ) {
        let account_id = account_delta.and_then(|delta| match delta {
            AccountUpdateDetails::Private => {
                tracing::warn!("ignoring private account details");
                None
            },
            AccountUpdateDetails::New(account) => {
                let account_id = account.id();
                let account = AccountState::new_inflight(account);
                self.accounts
                    .insert(account_id, account)
                    .inspect(|_| tracing::warn!(account.id = %account_id, "replaced an already existing account state"));

                Some(account_id)
            },
            AccountUpdateDetails::Delta(_account_delta) => {
                todo!("Waiting on account ID to be added here in base");
            },
        });

        // Filter out non-network and non-single-target notes.
        let mut associated_nullifiers = Vec::with_capacity(network_notes.len());
        for note in network_notes {
            let id = note.id();
            let Some(note) = NetworkNote::new(note) else {
                tracing::warn!(node.id = %id, "ignoring non-network note");
                continue;
            };

            if note.is_single_target().not() {
                tracing::warn!(node.id = %id, "ignoring multi-target network note");
                continue;
            }

            associated_nullifiers.push(note.nullifier());
            self.notes.insert(note.nullifier(), note);
        }

        self.inflight.insert(
            id,
            InflightTx {
                account_delta: account_id,
                nullifiers: nullifiers.clone(),
                notes: associated_nullifiers,
            },
        );
        self.nullifiers.extend(nullifiers);
    }

    fn revert_transactions(&mut self, txs: BTreeSet<TransactionId>) {
        for tx in txs {
            let Some(tx) = self.inflight.remove(&tx) else {
                continue;
            };

            if let Some(account_id) = tx.account_delta {
                let account = self
                    .accounts
                    .remove(&account_id)
                    .expect("account with delta should be tracked")
                    .revert_one();

                if let Some(account) = account {
                    self.accounts.insert(account_id, account);
                }

                for nullifier in tx.nullifiers {
                    self.nullifiers.remove(&nullifier);
                }

                for note in tx.notes {
                    self.notes.remove(&note);
                }
            }
        }
    }

    pub fn chain_tip(&self) -> BlockNumber {
        self.latest_header.block_num()
    }

    pub fn account_with_unconsumed_notes(&self) -> Option<(&Account, Iter<&NetworkNote>)> {
        todo!()
    }
}

/// Represents the impact an inflight transaction has on the state.
///
/// This is used to update state once the associated transaction is committed or reverted.
struct InflightTx {
    /// Which network account, if any, this transaction updated.
    ///
    /// Note that this could include creating a network account.
    account_delta: Option<AccountId>,
    /// Notes consumed by this transaction.
    nullifiers: Vec<Nullifier>,
    /// Notes created by this transaction. We use nullifiers to track these
    /// as this simplifies the state structure.
    notes: Vec<Nullifier>,
}

impl MastForestStore for State {
    fn get(
        &self,
        _procedure_hash: &miden_objects::Digest,
    ) -> Option<std::sync::Arc<miden_objects::MastForest>> {
        todo!()
    }
}

#[async_trait::async_trait(?Send)]
impl DataStore for State {
    async fn get_transaction_inputs(
        &self,
        _account_id: AccountId,
        _ref_blocks: BTreeSet<BlockNumber>,
    ) -> Result<(Account, Option<Word>, BlockHeader, PartialBlockchain), DataStoreError> {
        todo!();
    }
}

/// Max number of notes taken for executing a single network transaction
const MAX_BATCH: usize = 50;
