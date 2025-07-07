use std::{num::NonZeroUsize, sync::Mutex};

use lru::LruCache;
use miden_node_proto::domain::account::{NetworkAccountError, NetworkAccountPrefix};
use miden_objects::account::{Account, AccountId};

// ACCOUNT CACHE
// =================================================================================================

/// A capacity-limited network account cache.
///
/// The cache works in an LRU fashion and evicts accounts once capacity starts getting exceeded.
pub struct NetworkAccountCache {
    inner: Mutex<LruCache<NetworkAccountPrefix, Account>>,
}

impl NetworkAccountCache {
    /// Create a new cache with the given `capacity`.
    ///
    /// # Panics
    /// Panics if the capacity is `0`.
    pub fn new(capacity: NonZeroUsize) -> Self {
        let cache = LruCache::new(capacity);
        Self { inner: Mutex::new(cache) }
    }

    /// Insert or replace an account.
    ///
    /// # Errors
    ///
    /// - if `account` is not a network account.
    pub fn put(&self, account: &Account) -> Result<(), NetworkAccountError> {
        let prefix = NetworkAccountPrefix::try_from(account.id())?;
        self.inner.lock().expect("poisoned mutex").put(prefix, account.clone());
        Ok(())
    }

    /// Retrieve an account by prefix, refreshing its LRU position.
    pub fn get(&self, prefix: NetworkAccountPrefix) -> Option<Account> {
        self.inner.lock().expect("poisoned mutex").get(&prefix).cloned()
    }

    /// Manually evict a specific account.
    pub fn evict(&self, account_id: AccountId) -> Option<Account> {
        let prefix = NetworkAccountPrefix::try_from(account_id).ok()?;
        self.inner.lock().expect("poisoned mutex").pop(&prefix)
    }
}

// TESTS
// =================================================================================================

#[cfg(test)]
mod tests {
    use std::{num::NonZeroUsize, sync::Arc};

    use miden_lib::{account::auth::RpoFalcon512, transaction::TransactionKernel};
    use miden_objects::{
        EMPTY_WORD, Felt, account::Account, crypto::dsa::rpo_falcon512::PublicKey,
    };

    use crate::builder::data_store::NetworkAccountCache;

    #[test]
    fn insert_get() {
        let account = create_account(10);
        let cache = NetworkAccountCache::new(2.try_into().unwrap());

        cache.put(&account.clone()).unwrap();
        assert_eq!(cache.get(account.id().try_into().unwrap()).unwrap(), account);
    }

    #[test]
    fn lru_evicts_least_recently_used_account() {
        let cache = NetworkAccountCache::new(2.try_into().unwrap());
        let acc_id_1: u32 = 0x0100;
        let acc_id_2: u32 = 0x0200;
        let acc_id_3: u32 = 0x0300;

        let acc1 = create_account(acc_id_1);
        let acc2 = create_account(acc_id_2);
        let acc3 = create_account(acc_id_3);

        cache.put(&acc1.clone()).unwrap();
        cache.put(&acc2.clone()).unwrap();

        assert_eq!(cache.inner.lock().unwrap().len(), 2);

        assert!(cache.get(acc1.id().try_into().unwrap()).is_some());
        assert!(cache.get(acc2.id().try_into().unwrap()).is_some());

        // access the account to makr it as recently used
        cache.get(acc1.id().try_into().unwrap()).unwrap();

        // evict acc2
        cache.put(&acc3.clone()).unwrap();

        assert_eq!(cache.inner.lock().unwrap().len(), 2);

        assert!(cache.get(acc1.id().try_into().unwrap()).is_some());
        assert!(cache.get(acc2.id().try_into().unwrap()).is_none());
        assert!(cache.get(acc3.id().try_into().unwrap()).is_some());
    }

    #[test]
    #[should_panic]
    fn zero_capacity_panics() {
        let _ = NetworkAccountCache::new(0.try_into().unwrap());
    }

    #[test]
    fn update_existing_entry_doesnt_grow_cache() {
        let cache = NetworkAccountCache::new(1.try_into().unwrap());
        let acc = create_account(42);
        cache.put(&acc).unwrap();
        cache.put(&acc).unwrap();
        assert_eq!(cache.inner.lock().unwrap().len(), 1);
    }

    #[test]
    fn manual_evict_removes_entry() {
        let cache = NetworkAccountCache::new(2.try_into().unwrap());
        let acc = create_account(7);
        cache.put(&acc).unwrap();
        assert!(cache.evict(acc.id()).is_some());
        assert!(cache.get(acc.id().try_into().unwrap()).is_none());
    }

    #[test]
    fn concurrent_put_get() {
        let cache = Arc::new(NetworkAccountCache::new(10.try_into().unwrap()));
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
        let cache = NetworkAccountCache::new(3.try_into().unwrap());

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
        let cache = NetworkAccountCache::new(NonZeroUsize::new(capacity).unwrap());

        let accs: Vec<_> = (0..total).map(|i| create_account((i + 1) as u32)).collect();

        for a in &accs {
            cache.put(a).unwrap();
        }

        assert_eq!(cache.inner.lock().unwrap().len(), capacity);

        for acc in accs.iter().take(total - capacity) {
            assert!(cache.get(acc.id().try_into().unwrap()).is_none());
        }
        for acc in accs.iter().take(total).skip(total - capacity) {
            assert!(cache.get(acc.id().try_into().unwrap()).is_some());
        }
    }

    fn create_account(id: u32) -> Account {
        // network storage mode bits
        let storage_mode: u128 = 0b01;
        // NOTE: this shifts the ID to generate a different prefix
        Account::mock(
            (u128::from(id) << 99) | (storage_mode << 70),
            Felt::new(0),
            RpoFalcon512::new(PublicKey::new(EMPTY_WORD)),
            TransactionKernel::testing_assembler(),
        )
    }
}
