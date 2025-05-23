use std::{
    collections::{BTreeMap, HashMap},
    sync::Mutex,
};

use miden_node_utils::account::{NetworkAccountError, NetworkAccountPrefix};
use miden_objects::account::{Account, AccountId};

// ACCOUNT CACHE
// =================================================================================================

/// Represents an entry in the account cache, with a `generation` counter to evict older entries
struct AccountEntry {
    account: Account,
    generation: usize,
}

/// Cache state, containing a set of accounts mapped from their ID prefix as well as ordering,
/// used for LRU eviction.
struct CacheState {
    generation: usize,
    accounts: HashMap<NetworkAccountPrefix, AccountEntry>,
    ordering: BTreeMap<usize, NetworkAccountPrefix>,
}

/// A capacity-limited network account cache.
///
/// The cache works in an LRU fashion and evicts accounts once capacity starts getting exceeded.
pub struct NetworkAccountCache {
    capacity: usize,
    state: Mutex<CacheState>,
}

// TODO: polish this, maybe use an external crate for something more robust/performant
impl NetworkAccountCache {
    /// Creates a new [`AccountCache`]
    ///
    /// # Panics
    /// - if `capacity` is 0
    pub fn new(capacity: usize) -> Self {
        assert!((capacity != 0), "cache capacity cannot be 0");

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
    ///
    /// # Errors
    ///
    /// - If the account is not a network account
    pub fn put(&self, account: &Account) -> Result<(), NetworkAccountError> {
        let account_id = NetworkAccountPrefix::try_from(account.id())?;
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
        Ok(())
    }

    /// Get a reference to a value if it exists in the cache.
    /// Additionally, when there is a cache hit, marks the account as recently used.
    pub fn get(&self, prefix: NetworkAccountPrefix) -> Option<Account> {
        let mut state = self.state.lock().unwrap();

        let (old_generation, account) = {
            state.generation += 1;
            let new_generation = state.generation;
            let entry = state.accounts.get_mut(&prefix)?;
            let old = entry.generation;
            entry.generation = new_generation;
            (old, entry.account.clone())
        };

        let new_generation = state.generation;
        state.ordering.remove(&old_generation);
        state.ordering.insert(new_generation, prefix);

        Some(account)
    }

    /// Manually evict an entry from the cache.
    pub fn evict(&self, account_id: AccountId) -> Option<Account> {
        // Only evict if account ID is an actual network account ID
        if let Ok(prefix) = NetworkAccountPrefix::try_from(account_id) {
            let mut state = self.state.lock().unwrap();
            if let Some(entry) = state.accounts.remove(&prefix) {
                state.ordering.remove(&entry.generation);
                return Some(entry.account);
            }
        }
        None
    }
}

// TESTS
// =================================================================================================

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use miden_lib::transaction::TransactionKernel;
    use miden_objects::{Felt, account::Account};

    use crate::builder::data_store::NetworkAccountCache;

    #[test]
    fn insert_get() {
        let account = create_account(10);
        let cache = NetworkAccountCache::new(2);

        cache.put(&account.clone()).unwrap();
        assert_eq!(cache.get(account.id().try_into().unwrap()).unwrap(), account);
    }

    #[test]
    fn lru_evicts_least_recently_used_account() {
        let cache = NetworkAccountCache::new(2);
        let acc_id_1: u32 = 0x0100;
        let acc_id_2: u32 = 0x0200;
        let acc_id_3: u32 = 0x0300;

        let acc1 = create_account(acc_id_1);
        let acc2 = create_account(acc_id_2);
        let acc3 = create_account(acc_id_3);

        cache.put(&acc1.clone()).unwrap();
        cache.put(&acc2.clone()).unwrap();

        assert_eq!(cache.state.lock().unwrap().accounts.len(), 2);

        assert!(cache.get(acc1.id().try_into().unwrap()).is_some());
        assert!(cache.get(acc2.id().try_into().unwrap()).is_some());

        // access the account to makr it as recently used
        cache.get(acc1.id().try_into().unwrap()).unwrap();

        // evict acc2
        cache.put(&acc3.clone()).unwrap();

        assert_eq!(cache.state.lock().unwrap().accounts.len(), 2);

        assert!(cache.get(acc1.id().try_into().unwrap()).is_some());
        assert!(cache.get(acc2.id().try_into().unwrap()).is_none());
        assert!(cache.get(acc3.id().try_into().unwrap()).is_some());
    }

    #[test]
    #[should_panic]
    fn zero_capacity_panics() {
        let _ = NetworkAccountCache::new(0);
    }

    #[test]
    fn update_existing_entry_doesnt_grow_cache() {
        let cache = NetworkAccountCache::new(1);
        let acc = create_account(42);
        cache.put(&acc).unwrap();
        let first_gen = cache.state.lock().unwrap().generation;
        cache.put(&acc).unwrap();
        let state = cache.state.lock().unwrap();
        assert_eq!(state.accounts.len(), 1);
        assert!(state.generation > first_gen);
    }

    #[test]
    fn manual_evict_removes_entry() {
        let cache = NetworkAccountCache::new(2);
        let acc = create_account(7);
        cache.put(&acc).unwrap();
        assert!(cache.evict(acc.id()).is_some());
        assert!(cache.get(acc.id().try_into().unwrap()).is_none());
    }

    #[test]
    fn concurrent_put_get() {
        let cache = Arc::new(NetworkAccountCache::new(10));
        let handles: Vec<_> = (0..10)
            .map(|i| {
                let cache = cache.clone();
                std::thread::spawn(move || {
                    let acc = create_account(i + 1);
                    cache.put(&acc).unwrap();
                    let got = cache.get(acc.id().try_into().unwrap()).unwrap();
                    assert_eq!(got.id(), acc.id());
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn put_replaces_and_refreshes_lru() {
        let cache = NetworkAccountCache::new(3);

        let original = create_account(0xABC);
        let updated = create_account(0xABC);
        let other1 = create_account(0xDEF);
        let other2 = create_account(0xFED);

        cache.put(&original).unwrap();
        cache.put(&other1).unwrap();
        cache.put(&other2).unwrap();
        cache.put(&updated).unwrap();

        let newcomer = create_account(0x123);
        cache.put(&newcomer).unwrap();

        assert!(cache.get(original.id().try_into().unwrap()).is_some());
        assert!(cache.get(other1.id().try_into().unwrap()).is_none());
        assert!(cache.get(other2.id().try_into().unwrap()).is_some());
        assert!(cache.get(newcomer.id().try_into().unwrap()).is_some());
    }

    #[test]
    fn large_bulk_eviction() {
        let capacity = 300;
        let total: usize = 500;
        let cache = NetworkAccountCache::new(capacity);

        let accs: Vec<_> = (0..total).map(|i| create_account((i + 1) as u32)).collect();

        for a in &accs {
            cache.put(a).unwrap();
        }

        assert_eq!(cache.state.lock().unwrap().accounts.len(), capacity);

        for acc in accs.iter().take(total - capacity) {
            assert!(cache.get(acc.id().try_into().unwrap()).is_none());
        }
        for acc in accs.iter().take(total).skip(total - capacity) {
            assert!(cache.get(acc.id().try_into().unwrap()).is_some());
        }
    }

    fn create_account(id: u32) -> Account {
        // NOTE: this shifts the ID to generate a different prefix
        Account::mock(u128::from(id) << 99, Felt::new(0), TransactionKernel::testing_assembler())
    }
}
