/// NOTE: This module contains logic that will eventually be moved to the Validator component
/// when it is added to this repository.
use std::collections::BTreeSet;

use miden_protocol::Word;
use miden_protocol::account::{AccountId, PartialAccount, StorageMapWitness};
use miden_protocol::asset::{AssetVaultKey, AssetWitness};
use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::note::NoteScript;
use miden_protocol::transaction::{AccountInputs, PartialBlockchain, TransactionInputs};
use miden_protocol::vm::FutureMaybeSend;
use miden_tx::{DataStore, DataStoreError, MastForestStore, TransactionMastStore};

// TRANSACTION INPUTS DATA STORE
// ================================================================================================

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
        async move {
            if foreign_account_id == self.tx_inputs.account().id() {
                return Err(DataStoreError::Other {
                    error_msg: format!(
                        "requested account with id {foreign_account_id} is local, not foreign"
                    )
                    .into(),
                    source: None,
                });
            }

            let foreign_inputs = self
                .tx_inputs
                .read_foreign_account_inputs(foreign_account_id)
                .map_err(|err| DataStoreError::Other {
                    error_msg: format!("failed to read foreign account inputs: {err}").into(),
                    source: Some(Box::new(err)),
                })?;
            Ok(foreign_inputs)
        }
    }

    fn get_vault_asset_witnesses(
        &self,
        account_id: AccountId,
        vault_root: Word,
        vault_keys: BTreeSet<AssetVaultKey>,
    ) -> impl FutureMaybeSend<Result<Vec<AssetWitness>, DataStoreError>> {
        async move {
            if self.tx_inputs.account().vault().root() != vault_root {
                return Err(DataStoreError::Other {
                    error_msg: "vault root mismatch".into(),
                    source: None,
                });
            }

            // Get asset witnessess from local or foreign account.
            if self.tx_inputs.account().id() == account_id {
                get_asset_witnesses_from_account(self.tx_inputs.account(), vault_keys)
            } else {
                let foreign_inputs = self
                    .tx_inputs
                    .read_foreign_account_inputs(account_id)
                    .map_err(|err| DataStoreError::Other {
                        error_msg: format!("failed to read foreign account inputs: {err}").into(),
                        source: Some(Box::new(err)),
                    })?;
                get_asset_witnesses_from_account(foreign_inputs.account(), vault_keys)
            }
        }
    }

    fn get_storage_map_witness(
        &self,
        account_id: AccountId,
        map_root: Word,
        map_key: Word,
    ) -> impl FutureMaybeSend<Result<StorageMapWitness, DataStoreError>> {
        async move {
            if self.tx_inputs.account().id() == account_id {
                let storage_map_witness =
                    self.tx_inputs.account().storage().maps().find_map(|partial_map| {
                        if partial_map.root() == map_root {
                            partial_map.open(&map_key).ok()
                        } else {
                            None
                        }
                    });
                storage_map_witness
                    .ok_or_else(|| DataStoreError::Other { error_msg: "todo".into(), source: None })
            } else {
                // Get foreign account inputs.
                let foreign_inputs = self
                    .tx_inputs
                    .read_foreign_account_inputs(account_id)
                    .map_err(|_| DataStoreError::AccountNotFound(account_id))?;

                // Search through the foreign account's partial storage maps.
                let storage_map_witness =
                    foreign_inputs.account().storage().maps().find_map(|partial_map| {
                        if partial_map.root() == map_root {
                            partial_map.open(&map_key).ok()
                        } else {
                            None
                        }
                    });

                storage_map_witness
                    .ok_or_else(|| DataStoreError::Other { error_msg: "todo".into(), source: None })
            }
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
    fn get(&self, procedure_hash: &Word) -> Option<std::sync::Arc<miden_protocol::MastForest>> {
        self.mast_store.get(procedure_hash)
    }
}

// HELPERS
// ================================================================================================

fn get_asset_witnesses_from_account(
    account: &PartialAccount,
    vault_keys: BTreeSet<AssetVaultKey>,
) -> Result<Vec<AssetWitness>, DataStoreError> {
    Result::<Vec<_>, _>::from_iter(vault_keys.into_iter().map(|vault_key| {
        match account.vault().open(vault_key) {
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
