use std::sync::Mutex;

use miden_objects::{
    accounts::{Account, AccountId},
    notes::NoteId,
    transaction::{ChainMmr, InputNotes, TransactionInputs},
    BlockHeader, Word,
};
use miden_tx::{DataStore, DataStoreError};

use crate::errors::HandlerError;

pub struct FaucetDataStore {
    faucet_account: Mutex<Account>,
    /// Optional initial seed used for faucet account creation.
    init_seed: Option<Word>,
    block_header: BlockHeader,
    chain_mmr: ChainMmr,
}

// FAUCET DATA STORE
// ================================================================================================

impl FaucetDataStore {
    pub fn new(
        faucet_account: Account,
        init_seed: Option<Word>,
        block_header: BlockHeader,
        chain_mmr: ChainMmr,
    ) -> Self {
        Self {
            faucet_account: Mutex::new(faucet_account),
            init_seed,
            block_header,
            chain_mmr,
        }
    }

    /// Returns the stored faucet account.
    pub fn faucet_account(&self) -> Account {
        self.faucet_account.lock().expect("Poisoned lock").clone()
    }

    /// Updates the stored faucet account with the new one.
    pub async fn update_faucet_state(&self, new_faucet_state: Account) -> Result<(), HandlerError> {
        *self.faucet_account.lock().expect("Poisoned lock") = new_faucet_state;

        Ok(())
    }
}

impl DataStore for FaucetDataStore {
    fn get_transaction_inputs(
        &self,
        account_id: AccountId,
        _block_ref: u32,
        _notes: &[NoteId],
    ) -> Result<TransactionInputs, DataStoreError> {
        let account = self.faucet_account.lock().expect("Poisoned lock");
        if account_id != account.id() {
            return Err(DataStoreError::AccountNotFound(account_id));
        }

        TransactionInputs::new(
            account.clone(),
            account.is_new().then_some(self.init_seed).flatten(),
            self.block_header,
            self.chain_mmr.clone(),
            InputNotes::default(),
        )
        .map_err(DataStoreError::InvalidTransactionInput)
    }
}
