use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{Arc, Mutex},
};

use miden_objects::{
    account::{Account, AccountId},
    block::{BlockHeader, BlockNumber},
    crypto::merkle::PartialMmr,
    transaction::PartialBlockchain,
    MastForest, Word,
};
use miden_tx::{DataStore, DataStoreError, MastForestStore, TransactionMastStore};

use super::store::StoreClient;

// DATA STORE
// =================================================================================================

pub struct NtxBuilderDataStore {
    store_client: StoreClient,
    mast_forest_store: TransactionMastStore,
    account_cache: AccountCache,
    block_ref: BlockHeader,
}

impl NtxBuilderDataStore {
    pub fn new(store_client: StoreClient, block_ref: BlockHeader) -> Self {
        let account_cache = AccountCache::new(128);
        let mast_forest_store = TransactionMastStore::new();
        Self {
            store_client,
            account_cache,
            block_ref,
            mast_forest_store,
        }
    }

    pub fn set_block_ref(&mut self, block_ref: BlockHeader) {
        self.block_ref = block_ref;
    }
}

#[async_trait::async_trait(?Send)]
impl DataStore for NtxBuilderDataStore {
    async fn get_transaction_inputs(
        &self,
        account_id: AccountId,
        _ref_blocks: BTreeSet<BlockNumber>,
    ) -> Result<(Account, Option<Word>, BlockHeader, PartialBlockchain), DataStoreError> {
        let account = if let Some(acc) = self.account_cache.get(account_id) {
            acc
        } else {
            let acc = self
                .store_client
                .get_network_account(account_id)
                .await
                .map_err(|_| DataStoreError::AccountNotFound(account_id))?;
            // cache account
            self.account_cache.put(&acc);

            acc
        };

        let peaks = self
            .store_client
            .get_mmr_peaks(self.block_ref.block_num())
            .await
            .map_err(|err| DataStoreError::other_with_source("error retrieving MMR peaks", err))?;

        let partial_mmr = PartialMmr::from_peaks(peaks);
        let partial_blockchain = PartialBlockchain::new(partial_mmr, []).unwrap();
        Ok((account, None, self.block_ref.clone(), partial_blockchain))
    }
}

impl MastForestStore for NtxBuilderDataStore {
    fn get(&self, procedure_hash: &miden_objects::Digest) -> Option<Arc<MastForest>> {
        self.mast_forest_store.get(procedure_hash)
    }
}

// ACCOUNT CACHE
// =================================================================================================

struct AccountEntry {
    account: Account,
    generation: usize,
}

struct CacheState {
    generation: usize,
    accounts: HashMap<u128, AccountEntry>,
    ordering: BTreeMap<usize, AccountId>,
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
        let account_id = account.id();
        let mut state = self.state.lock().unwrap();
        state.generation += 1;
        let current_generation = state.generation;

        let removed_generation = if let Some(entry) = state.accounts.insert(
            account_id.into(),
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
                state.accounts.remove(&oldest_key.into());
            }
        }
    }

    /// Get a reference to a value if it exists in the cache.
    /// Additionally, when there is a cache hit, marks the account as recently used.
    pub fn get(&self, account_id: AccountId) -> Option<Account> {
        let mut state = self.state.lock().unwrap();

        let (old_generation, account_clone) = {
            state.generation += 1;
            let new_generation = state.generation;
            let entry = state.accounts.get_mut(&account_id.into())?;
            let old = entry.generation;
            entry.generation = new_generation;
            (old, entry.account.clone())
        };

        let new_generation = state.generation;
        state.ordering.remove(&old_generation);
        state.ordering.insert(new_generation, account_id);

        Some(account_clone)
    }

    pub fn len(&self) -> usize {
        self.state.lock().unwrap().accounts.len()
    }
}

// TESTS
// =================================================================================================

#[cfg(test)]
mod tests {
    use miden_lib::transaction::TransactionKernel;
    use miden_objects::{account::Account, Felt};

    use crate::builder::data_store::AccountCache;

    fn create_account(id: u128) -> Account {
        Account::mock(id, Felt::new(0), TransactionKernel::testing_assembler())
    }

    #[test]
    fn insert_get() {
        let account = create_account(10);
        let cache = AccountCache::new(2);

        cache.put(&account.clone());
        assert_eq!(cache.get(account.id()).unwrap(), account);
    }

    #[test]
    fn lru_evicts_least_recently_used_account() {
        let cache = AccountCache::new(2);
        let acc1 = create_account(0x11111);
        let acc2 = create_account(0x22222);
        let acc3 = create_account(0x33333);

        cache.put(&acc1.clone());
        cache.put(&acc2.clone());

        assert_eq!(cache.len(), 2);

        assert!(cache.get(acc1.id()).is_some());
        assert!(cache.get(acc2.id()).is_some());

        cache.get(acc1.id()).unwrap();
        cache.put(&acc3.clone());

        assert_eq!(cache.len(), 2);
        assert!(cache.get(acc1.id()).is_some());
        assert!(cache.get(acc2.id()).is_none());
        assert!(cache.get(acc3.id()).is_some());
    }
}
