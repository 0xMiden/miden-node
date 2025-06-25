use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ops::Not,
    slice::Iter,
};

use miden_node_proto::domain::mempool::MempoolEvent;
use miden_objects::{
    Word,
    account::{Account, AccountDelta, AccountId, delta::AccountUpdateDetails},
    block::{BlockHeader, BlockNumber},
    note::{Note, Nullifier},
    transaction::{PartialBlockchain, TransactionId},
};
use miden_tx::{DataStore, DataStoreError, MastForestStore};

use crate::note::NetworkNote;
use crate::store::StoreClient;

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
            inflight: Default::default(),
            accounts: Default::default(),
            notes: Default::default(),
            nullifiers: Default::default(),
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
                // TODO: AccountUpdateDetails is the wrong type to use.
                //
                // TODO: Update AccountState
                let account_id = account_delta.map(|delta| match delta {
                    AccountUpdateDetails::Private => unreachable!("Should never occur"),
                    AccountUpdateDetails::New(account) => account.id(),
                    AccountUpdateDetails::Delta(_account_delta) => {
                        todo!("This is the wrong type to use here, we need an account ID")
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
            },
            MempoolEvent::BlockCommitted { header, txs } => {
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
                    };

                    for nullifier in tx.nullifiers {
                        self.notes.remove(&nullifier);
                        self.nullifiers.remove(&nullifier);
                    }
                }
            },
            MempoolEvent::TransactionsReverted(txs) => {
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
                    };
                }
            },
        }
    }

    pub fn chain_tip(&self) -> BlockNumber {
        self.latest_header.block_num()
    }

    pub fn account_with_unconsumed_notes(&self) -> Option<(&Account, Iter<&NetworkNote>)> {
        todo!()
    }
}

struct InflightTx {
    account_delta: Option<AccountId>,
    nullifiers: Vec<Nullifier>,
    notes: Vec<Nullifier>,
}

struct AccountState {
    state: Account,
    deltas: VecDeque<AccountDelta>,
}

impl AccountState {
    fn new(state: Account) -> Self {
        Self { state, deltas: Default::default() }
    }

    fn commit_one(&mut self) {
        let Some(to_commit) = self.deltas.pop_front() else {
            panic!("account state should have a delta to commit");
        };

        self.state.apply_delta(&to_commit).expect("account delta should apply");
    }

    fn revert_one(mut self) -> Option<Self> {
        self.deltas.pop_back().is_some().then_some(self)
    }

    fn current_state(&self) -> Account {
        let mut account = self.state.clone();

        for delta in &self.deltas {
            account.apply_delta(delta).expect("account delta should apply");
        }

        account
    }
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
