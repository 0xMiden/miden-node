/// NOTE: This module contains logic that will eventually be moved to the Validator component
/// when it is added to this repository.
use std::collections::BTreeSet;

use miden_objects::Word;
use miden_objects::account::{AccountId, PartialAccount, StorageMapWitness};
use miden_objects::asset::{AssetVaultKey, AssetWitness};
use miden_objects::block::{BlockHeader, BlockNumber};
use miden_objects::note::NoteScript;
use miden_objects::transaction::{AccountInputs, PartialBlockchain, TransactionInputs};
use miden_objects::vm::FutureMaybeSend;
use miden_tx::{DataStore, DataStoreError, MastForestStore, TransactionMastStore};

/// A [`DataStore`] implementation that wraps [`TransactionInputs`]
pub struct TransactionInputsDataStore {
    tx_inputs: TransactionInputs,
    mast_store: TransactionMastStore,
}

impl TransactionInputsDataStore {
    pub fn new(tx_inputs: TransactionInputs) -> Self {
        let mast_store = TransactionMastStore::new();
        mast_store.load_account_code(tx_inputs.account().code());
        Self { tx_inputs, mast_store }
    }
}

impl DataStore for TransactionInputsDataStore {
    fn get_transaction_inputs(
        &self,
        account_id: AccountId,
        _ref_blocks: BTreeSet<BlockNumber>,
    ) -> impl FutureMaybeSend<Result<(PartialAccount, BlockHeader, PartialBlockchain), DataStoreError>>
    {
        async move {
            if self.tx_inputs.account().id() != account_id {
                return Err(DataStoreError::AccountNotFound(account_id));
            }

            Ok((
                self.tx_inputs.account().clone(),
                self.tx_inputs.block_header().clone(),
                self.tx_inputs.blockchain().clone(),
            ))
        }
    }

    fn get_foreign_account_inputs(
        &self,
        foreign_account_id: AccountId,
        _ref_block: BlockNumber,
    ) -> impl FutureMaybeSend<Result<AccountInputs, DataStoreError>> {
        async move { Err(DataStoreError::AccountNotFound(foreign_account_id)) }
    }

    fn get_vault_asset_witnesses(
        &self,
        account_id: AccountId,
        vault_root: Word,
        vault_keys: BTreeSet<AssetVaultKey>,
    ) -> impl FutureMaybeSend<Result<Vec<AssetWitness>, DataStoreError>> {
        async move {
            if self.tx_inputs.account().id() != account_id {
                return Err(DataStoreError::AccountNotFound(account_id));
            }

            if self.tx_inputs.account().vault().root() != vault_root {
                return Err(DataStoreError::Other {
                    error_msg: "vault root mismatch".into(),
                    source: None,
                });
            }

            Result::<Vec<_>, _>::from_iter(vault_keys.into_iter().map(|vault_key| {
                match self.tx_inputs.account().vault().open(vault_key) {
                    Ok(vault_proof) => {
                        AssetWitness::new(vault_proof.into()).map_err(|err| DataStoreError::Other {
                            error_msg: "failed to open vault asset tree".into(),
                            source: Some(err.into()),
                        })
                    },
                    Err(err) => Err(DataStoreError::Other {
                        error_msg: "failed to open vault".into(),
                        source: Some(err.into()),
                    }),
                }
            }))
        }
    }

    fn get_storage_map_witness(
        &self,
        account_id: AccountId,
        _map_root: Word,
        _map_key: Word,
    ) -> impl FutureMaybeSend<Result<StorageMapWitness, DataStoreError>> {
        async move {
            if self.tx_inputs.account().id() != account_id {
                return Err(DataStoreError::AccountNotFound(account_id));
            }

            // For partial accounts, storage map witness is not available.
            Err(DataStoreError::Other {
                error_msg: "storage map witness not available with partial account state".into(),
                source: None,
            })
        }
    }

    fn get_note_script(
        &self,
        _script_root: Word,
    ) -> impl FutureMaybeSend<Result<Option<NoteScript>, DataStoreError>> {
        async move { Ok(None) }
    }
}

impl MastForestStore for TransactionInputsDataStore {
    fn get(&self, procedure_hash: &Word) -> Option<std::sync::Arc<miden_objects::MastForest>> {
        self.mast_store.get(procedure_hash)
    }
}
