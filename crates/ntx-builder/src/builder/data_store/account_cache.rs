use std::{
    collections::{BTreeMap, HashMap},
    sync::Mutex,
};

use miden_objects::{
    account::{Account, AccountId},
    note::{NoteExecutionMode, NoteTag},
};

// ACCOUNT CACHE
// =================================================================================================

#[derive(Debug, Hash, Clone, Copy, PartialEq, Eq)]
pub struct NtxAccountIdPrefix(u32);

impl From<AccountId> for NtxAccountIdPrefix {
    fn from(id: AccountId) -> Self {
        NtxAccountIdPrefix((id.prefix().as_u64() >> 34) as u32)
    }
}

impl TryFrom<NoteTag> for NtxAccountIdPrefix {
    type Error = String;

    fn try_from(tag: NoteTag) -> Result<Self, Self::Error> {
        if tag.execution_mode() == NoteExecutionMode::Network && tag.is_single_target() {
            return Ok(NtxAccountIdPrefix(tag.inner()));
        }
        Err("tag not meant for executing by a single account".into())
    }
}

struct AccountEntry {
    account: Account,
    generation: usize,
}

struct CacheState {
    generation: usize,
    accounts: HashMap<NtxAccountIdPrefix, AccountEntry>,
    ordering: BTreeMap<usize, NtxAccountIdPrefix>,
}

pub struct AccountCache {
    capacity: usize,
    state: Mutex<CacheState>,
}

impl AccountCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            state: std::sync::Mutex::new(CacheState {
                generation: 0,
                accounts: HashMap::with_capacity(capacity),
                ordering: BTreeMap::new(),
            }),
        }
    }

    /// Insert or replace an entry.
    pub fn put(&self, account: &Account) {
        let account_id = NtxAccountIdPrefix::from(account.id());
        let mut state = self.state.lock().unwrap();
        state.generation += 1;
        let current_generation = state.generation;

        let removed_generation = if let Some(entry) = state.accounts.insert(
            account_id,
            AccountEntry {
                generation: current_generation,
                account: account.clone(),
            },
        ) {
            Some(entry.generation)
        } else {
            None
        };

        if let Some(old) = removed_generation {
            state.ordering.remove(&old);
        }
        state.ordering.insert(current_generation, account_id);

        if state.accounts.len() > self.capacity {
            if let Some((_, oldest_key)) = state.ordering.pop_first() {
                state.accounts.remove(&oldest_key);
            }
        }
    }

    /// Get a reference to a value if it exists in the cache.
    /// Additionally, when there is a cache hit, marks the account as recently used.
    pub fn get(&self, account_id: NtxAccountIdPrefix) -> Option<Account> {
        let mut state = self.state.lock().unwrap();

        let (old_generation, account_clone) = {
            state.generation += 1;
            let new_generation = state.generation;
            let entry = state.accounts.get_mut(&account_id)?;
            let old = entry.generation;
            entry.generation = new_generation;
            (old, entry.account.clone())
        };

        let new_generation = state.generation;
        state.ordering.remove(&old_generation);
        state.ordering.insert(new_generation, account_id);

        Some(account_clone)
    }

    /// Manually evict an entry from the cache.
    pub fn evict(&self, account_id: AccountId) -> Option<Account> {
        let mut state = self.state.lock().unwrap();
        if let Some(entry) = state.accounts.remove(&account_id.into()) {
            state.ordering.remove(&entry.generation);
            return Some(entry.account);
        }
        None
    }
}

// TESTS
// =================================================================================================

#[cfg(test)]
mod tests {
    use miden_lib::transaction::TransactionKernel;
    use miden_objects::{Felt, account::Account};

    use crate::builder::data_store::AccountCache;

    fn create_account(id: u128) -> Account {
        Account::mock(id, Felt::new(0), TransactionKernel::testing_assembler())
    }

    #[test]
    fn insert_get() {
        let account = create_account(10);
        let cache = AccountCache::new(2);

        cache.put(&account.clone());
        assert_eq!(cache.get(account.id().into()).unwrap(), account);
    }

    #[test]
    fn lru_evicts_least_recently_used_account() {
        let cache = AccountCache::new(2);
        let acc_id_1: u128 = 0x0100_0000_0000_0000_0000_0000_0000_0000;
        let acc_id_2: u128 = 0x0200_0000_0000_0000_0000_0000_0000_0000;
        let acc_id_3: u128 = 0x0300_0000_0000_0000_0000_0000_0000_0000;

        let acc1 = create_account(acc_id_1);
        let acc2 = create_account(acc_id_2);
        let acc3 = create_account(acc_id_3);

        cache.put(&acc1.clone());
        cache.put(&acc2.clone());

        assert_eq!(cache.state.lock().unwrap().accounts.len(), 2);

        assert!(cache.get(acc1.id().into()).is_some());
        assert!(cache.get(acc2.id().into()).is_some());

        // access the account to makr it as recently used
        cache.get(acc1.id().into()).unwrap();

        // evict acc2
        cache.put(&acc3.clone());

        assert_eq!(cache.state.lock().unwrap().accounts.len(), 2);

        assert!(cache.get(acc1.id().into()).is_some());
        assert!(cache.get(acc2.id().into()).is_none());
        assert!(cache.get(acc3.id().into()).is_some());
    }
}
