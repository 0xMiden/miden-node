use std::{collections::BTreeSet, sync::Mutex};

use miden_objects::{
    MastForest, Word,
    account::{Account, AccountId},
    block::{BlockHeader, BlockNumber},
    transaction::PartialBlockchain,
};
use miden_tx::{DataStore, DataStoreError, MastForestStore, TransactionMastStore};
use winter_maybe_async::maybe_async_trait;

pub struct FaucetDataStore {
    faucet_account: Mutex<Account>,
    /// Optional initial seed used for faucet account creation.
    init_seed: Option<Word>,
    block_header: BlockHeader,
    partial_block_chain: PartialBlockchain,
    mast_store: TransactionMastStore,
}

// FAUCET DATA STORE
// ================================================================================================

impl FaucetDataStore {
    pub fn new(
        faucet_account: Account,
        init_seed: Option<Word>,
        block_header: BlockHeader,
        partial_block_chain: PartialBlockchain,
    ) -> Self {
        let mast_store = TransactionMastStore::new();
        mast_store.insert(faucet_account.code().mast());

        Self {
            faucet_account: Mutex::new(faucet_account),
            init_seed,
            block_header,
            partial_block_chain,
            mast_store,
        }
    }

    /// Returns the stored faucet account.
    pub fn faucet_account(&self) -> Account {
        self.faucet_account.lock().expect("Poisoned lock").clone()
    }

    /// Updates the stored faucet account with the new one.
    pub fn update_faucet_state(&self, new_faucet_state: Account) {
        *self.faucet_account.lock().expect("Poisoned lock") = new_faucet_state;
    }
}

#[maybe_async_trait(?Send)]
impl DataStore for FaucetDataStore {
    #[maybe_async]
    fn get_transaction_inputs(
        &self,
        account_id: AccountId,
        _ref_blocks: BTreeSet<BlockNumber>,
    ) -> Result<(Account, Option<Word>, BlockHeader, PartialBlockchain), DataStoreError> {
        let account = self.faucet_account.lock().expect("Poisoned lock");
        if account_id != account.id() {
            return Err(DataStoreError::AccountNotFound(account_id));
        }

        Ok((
            account.clone(),
            account.is_new().then_some(self.init_seed).flatten(),
            self.block_header.clone(),
            self.partial_block_chain.clone(),
        ))
    }
}

impl MastForestStore for FaucetDataStore {
    fn get(&self, procedure_hash: &miden_objects::Digest) -> Option<std::sync::Arc<MastForest>> {
        self.mast_store.get(procedure_hash)
    }
}
