use std::{collections::BTreeSet, sync::Arc};

use account_cache::NetworkAccountCache;
use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_objects::{
    AccountError, MastForest, Word,
    account::{Account, AccountId},
    block::{BlockHeader, BlockNumber},
    crypto::merkle::PartialMmr,
    note::{NoteScript, NoteTag},
    transaction::{ExecutedTransaction, PartialBlockchain},
};
use miden_tx::{DataStore, DataStoreError, MastForestStore, TransactionMastStore};
use tokio::sync::Mutex;
use tracing::warn;

use super::NtxBuilderError;
use crate::{
    COMPONENT,
    store::{StoreClient, StoreError},
};

mod account_cache;

// DATA STORE
// =================================================================================================

/// Network Transaction Builder data store.
///
/// [`NtxBuilderDataStore`] implements [`DataStore`] and provides all input data required by the NTB
/// transaction executor. It maintains both a partial MMR and up-to-date account data. Accounts are
/// kept fresh via an LRU cache, falling back to the backing store on a miss. The partial MMR is
/// refreshed from the store whenever it becomes outdated.
pub struct NtxBuilderDataStore {
    mast_forest_store: TransactionMastStore,
    store_client: StoreClient,
    account_cache: NetworkAccountCache,
    block_ref: Mutex<BlockHeader>,
    partial_mmr: Mutex<PartialMmr>,
}

impl NtxBuilderDataStore {
    /// Creates a new [`NtxBuilderDataStore`].
    pub async fn new(store_client: StoreClient) -> Result<Self, NtxBuilderError> {
        let account_cache = NetworkAccountCache::new(128.try_into().expect("not zero"));
        let mast_forest_store = TransactionMastStore::new();

        // SAFETY: OK to unwrap because passing `None` should return the latest data everytime
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

    /// Inserts a note script's MAST forest into the MAST forest store.
    pub fn insert_note_script_mast(&self, note_script: &NoteScript) {
        self.mast_forest_store.insert(note_script.mast());
    }

    /// Retrieves the network account associated with a note tag and updates the state of the data
    /// store accordingly to be able to execute transactions with the account.
    /// Specifically, updating the state involves:
    ///
    /// - Retrieving the network account from the cache or through the Store and updating the cache
    /// - Loading the account's MAST forest into the data store's MAST forest
    pub async fn get_cached_acc_or_fetch_by_tag(
        &self,
        note_tag: NoteTag,
    ) -> Result<Option<Account>, NtxBuilderError> {
        let account_prefix: NetworkAccountPrefix = note_tag.try_into().unwrap();
        // Look in cache, try the store otherwise
        let account = if let Some(acc) = self.account_cache.get(account_prefix) {
            Some(acc)
        } else if let Some(acc) = self.store_client.get_network_account_by_tag(note_tag).await? {
            // Insert to cache
            self.account_cache.put(&acc)?;
            Some(acc)
        } else {
            None
        };

        if let Some(acc) = &account {
            self.mast_forest_store.insert(acc.code().mast());
        }

        Ok(account)
    }

    /// Updates the blockchain-related data by retrieving new block header data and MMR peaks
    /// from the store.
    pub async fn update_blockchain_data(&self) -> Result<BlockNumber, StoreError> {
        let current_block = { self.block_ref.lock().await.block_num() };

        let query_response =
            self.store_client.get_current_blockchain_data(Some(current_block)).await?;

        let new_block_num = if let Some((header, mmr)) = query_response {
            let mut block_ref = self.block_ref.lock().await;
            let mut partial_mmr = self.partial_mmr.lock().await;

            *block_ref = header;
            *partial_mmr = mmr;

            block_ref.block_num()
        } else {
            current_block
        };

        Ok(new_block_num)
    }

    /// Evict an account from the cache. This forces the NTB to retrieve account details from the
    /// store the next time a transaction is built for this account.
    pub fn evict_account(&self, account_id: AccountId) {
        self.account_cache.evict(account_id);
    }

    /// After a successful execution, updates the account cache with the new account details.
    pub fn update_account(&self, transaction: &ExecutedTransaction) -> Result<(), AccountError> {
        // SAFETY: datastore impl checks that the account ID is a valid network account
        let account_id_prefix = transaction.account_id().try_into().unwrap();
        let Some(mut account) = self.account_cache.get(account_id_prefix) else {
            warn!(target:COMPONENT, "account was expected to be found in the cache");
            return Ok(());
        };
        account.apply_delta(transaction.account_delta())?;
        self.account_cache.put(&account).unwrap();
        Ok(())
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
        assert_eq!(*block_num, self.block_ref.lock().await.block_num());

        let Ok(account_id_prefix) = NetworkAccountPrefix::try_from(account_id) else {
            return Err(DataStoreError::other("account is not a valid network account"));
        };

        let Some(account) = self.account_cache.get(account_id_prefix) else {
            return Err(DataStoreError::other(
                "account not found in cache; should have been retrieved before execution",
            ));
        };

        let partial_blockchain =
            PartialBlockchain::new(self.partial_mmr.lock().await.clone(), []).unwrap();

        Ok((account, None, self.block_ref.lock().await.clone(), partial_blockchain))
    }
}

impl MastForestStore for NtxBuilderDataStore {
    fn get(&self, procedure_hash: &miden_objects::Digest) -> Option<Arc<MastForest>> {
        self.mast_forest_store.get(procedure_hash)
    }
}
