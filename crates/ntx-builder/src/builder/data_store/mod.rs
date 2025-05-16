use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

use account_cache::AccountCache;
use miden_objects::{
    MastForest, Word,
    account::{Account, AccountId},
    block::{BlockHeader, BlockNumber},
    crypto::merkle::PartialMmr,
    note::NoteTag,
    transaction::PartialBlockchain,
};
use miden_tx::{DataStore, DataStoreError, MastForestStore, TransactionMastStore};

use super::store::{StoreClient, StoreError};

mod account_cache;

// DATA STORE
// =================================================================================================

pub struct NtxBuilderDataStore {
    mast_forest_store: TransactionMastStore,
    store_client: StoreClient,
    account_cache: AccountCache,
    block_ref: Mutex<BlockHeader>,
    partial_mmr: Mutex<PartialMmr>,
}

impl NtxBuilderDataStore {
    pub async fn new(store_client: StoreClient) -> Result<Self, StoreError> {
        let account_cache = AccountCache::new(128);
        let mast_forest_store = TransactionMastStore::new();

        // SAFETY: ok to unwrap because passing `None` should return the latest data everytime
        let (block_ref, partial_mmr) =
            store_client.get_current_blockchain_data(None).await?.unwrap();

        Ok(Self {
            account_cache,
            mast_forest_store,
            store_client,
            block_ref: Mutex::new(block_ref),
            partial_mmr: Mutex::new(partial_mmr),
        })
    }

    pub async fn get_cached_acc_or_fetch_by_tag(
        &self,
        note_tag: NoteTag,
    ) -> Result<Account, StoreError> {
        // SAFETY: we are trusting that notes from the store were correctly filtered
        if let Some(acc) = self.account_cache.get(note_tag.try_into().unwrap()) {
            Ok(acc)
        } else {
            let account = self.store_client.get_network_account_by_tag(note_tag).await?;
            // insert to cache
            self.account_cache.put(&account);
            Ok(account)
        }
    }

    pub async fn update_blockchain_data(&self) -> Result<BlockNumber, StoreError> {
        let current_block = self.block_ref.lock().unwrap().block_num();

        let query_response =
            self.store_client.get_current_blockchain_data(Some(current_block)).await?;

        if let Some((header, mmr)) = query_response {
            *self.block_ref.lock().unwrap() = header;
            *self.partial_mmr.lock().unwrap() = mmr;
        }

        Ok(self.block_ref.lock().unwrap().block_num())
    }

    pub fn evict_account(&self, account_id: AccountId) {
        self.account_cache.evict(account_id);
    }
}

#[async_trait::async_trait(?Send)]
impl DataStore for NtxBuilderDataStore {
    async fn get_transaction_inputs(
        &self,
        account_id: AccountId,
        ref_blocks: BTreeSet<BlockNumber>,
    ) -> Result<(Account, Option<Word>, BlockHeader, PartialBlockchain), DataStoreError> {
        // SAFETY: We can unwrap here because the executor always passes the reference block
        let block_num = ref_blocks.first().unwrap();
        assert_eq!(*block_num, self.block_ref.lock().unwrap().block_num());

        let Some(account) = self.account_cache.get(account_id.into()) else {
            return Err(DataStoreError::other(
                "account not found in cache; should have been retrieved before execution",
            ));
        };

        let partial_blockchain =
            PartialBlockchain::new(self.partial_mmr.lock().unwrap().clone(), []).unwrap();

        Ok((account, None, self.block_ref.lock().unwrap().clone(), partial_blockchain))
    }
}

impl MastForestStore for NtxBuilderDataStore {
    fn get(&self, procedure_hash: &miden_objects::Digest) -> Option<Arc<MastForest>> {
        self.mast_forest_store.get(procedure_hash)
    }
}
