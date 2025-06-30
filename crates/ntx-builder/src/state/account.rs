use std::collections::{BTreeMap, HashMap, VecDeque};

use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_objects::{
    account::{Account, AccountDelta, AccountId, delta::AccountUpdateDetails},
    transaction::TransactionId,
};

#[derive(Default)]
pub struct AccountStates {
    deltas: HashMap<NetworkAccountPrefix, VecDeque<AccountUpdate>>,
    txs: BTreeMap<TransactionId, NetworkAccountPrefix>,
}

pub enum AccountUpdate {
    New(Account),
    Delta(AccountDelta),
}

impl AccountStates {
    pub fn get(&self, account: &NetworkAccountPrefix) -> Option<&VecDeque<AccountUpdate>> {
        self.deltas.get(account)
    }

    pub fn add(&mut self, tx: TransactionId, update: AccountUpdateDetails) {
        let update = match update {
            AccountUpdateDetails::Private => {
                tracing::warn!("ignoring private network account update");
                return;
            },
            AccountUpdateDetails::New(account) => AccountUpdate::New(account),
            AccountUpdateDetails::Delta(account_delta) => AccountUpdate::Delta(account_delta),
        };

        let Ok(account) = NetworkAccountPrefix::try_from(update.account_id()) else {
            tracing::warn!("ignoring non-network account update");
            return;
        };

        self.deltas.entry(account).or_default().push_back(update);
        self.txs.insert(tx, account);
    }

    pub fn commit(&mut self, tx: TransactionId) {
        self.tx_update(tx, TxUpdate::Commit);
    }

    pub fn revert(&mut self, tx: TransactionId) {
        self.tx_update(tx, TxUpdate::Revert);
    }

    fn tx_update(&mut self, tx: TransactionId, change: TxUpdate) {
        let Some(account) = self.txs.remove(&tx) else {
            return;
        };

        let deltas = self.deltas.get_mut(&account).unwrap();
        match change {
            TxUpdate::Commit => deltas.pop_front(),
            TxUpdate::Revert => deltas.pop_back(),
        };
        if deltas.is_empty() {
            self.deltas.remove(&account);
        }
    }
}

enum TxUpdate {
    Commit,
    Revert,
}

impl AccountUpdate {
    fn account_id(&self) -> AccountId {
        match self {
            AccountUpdate::New(account) => account.id(),
            AccountUpdate::Delta(_account_delta) => todo!("Waiting on miden-base"),
        }
    }
}
