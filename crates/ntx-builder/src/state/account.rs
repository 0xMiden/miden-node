use std::collections::{BTreeMap, HashMap, VecDeque};

use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_objects::{
    account::{Account, AccountDelta, AccountId, delta::AccountUpdateDetails},
    transaction::TransactionId,
};

/// Tracks network account deltas for all currently inflight transactions.
#[derive(Default)]
pub struct AccountDeltas {
    deltas: HashMap<NetworkAccountPrefix, VecDeque<NetworkAccountUpdate>>,
    txs: BTreeMap<TransactionId, NetworkAccountPrefix>,
}

#[derive(Clone)]
pub enum NetworkAccountUpdate {
    New(Account),
    Delta(AccountDelta),
}

impl AccountDeltas {
    /// Returns the account delta's for the account, if any.
    pub fn get(&self, account: NetworkAccountPrefix) -> Option<&VecDeque<NetworkAccountUpdate>> {
        self.deltas.get(&account)
    }

    /// Tracks a new transaction and its account delta.
    ///
    /// Non-network account updates are ignored.
    pub fn add(
        &mut self,
        tx: TransactionId,
        update: AccountUpdateDetails,
    ) -> Option<NetworkAccountPrefix> {
        let update = match update {
            AccountUpdateDetails::Private => {
                tracing::warn!("ignoring private network account update");
                return None;
            },
            AccountUpdateDetails::New(account) => NetworkAccountUpdate::New(account),
            AccountUpdateDetails::Delta(account_delta) => {
                NetworkAccountUpdate::Delta(account_delta)
            },
        };

        let Ok(account) = NetworkAccountPrefix::try_from(update.account_id()) else {
            tracing::warn!("ignoring non-network account update");
            return None;
        };

        self.deltas.entry(account).or_default().push_back(update);
        self.txs.insert(tx, account);

        Some(account)
    }

    /// The transaction and its account delta is removed.
    pub fn commit(&mut self, tx: &TransactionId) {
        self.tx_update(tx, TxUpdate::Commit);
    }

    /// The transaction and its account delta is removed.
    pub fn revert(&mut self, tx: &TransactionId) {
        self.tx_update(tx, TxUpdate::Revert);
    }

    fn tx_update(&mut self, tx: &TransactionId, change: TxUpdate) {
        let Some(account) = self.txs.remove(tx) else {
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum TxUpdate {
    Commit,
    Revert,
}

impl NetworkAccountUpdate {
    fn account_id(&self) -> AccountId {
        match self {
            NetworkAccountUpdate::New(account) => account.id(),
            NetworkAccountUpdate::Delta(account_delta) => account_delta.id(),
        }
    }
}
