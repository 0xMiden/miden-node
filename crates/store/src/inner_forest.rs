use std::collections::BTreeMap;

use miden_protocol::account::delta::{AccountDelta, AccountStorageDelta, AccountVaultDelta};
use miden_protocol::account::{AccountId, NonFungibleDeltaAction, StorageSlotName};
use miden_protocol::asset::{Asset, FungibleAsset};
use miden_protocol::block::BlockNumber;
use miden_protocol::crypto::merkle::EmptySubtreeRoots;
use miden_protocol::crypto::merkle::smt::{SMT_DEPTH, SmtForest};
use miden_protocol::{EMPTY_WORD, Word};

#[cfg(test)]
mod tests;

/// Container for forest-related state that needs to be updated atomically.
pub(crate) struct InnerForest {
    /// `SmtForest` for efficient account storage reconstruction.
    /// Populated during block import with storage and vault SMTs.
    pub(crate) storage_forest: SmtForest,

    /// Maps (`account_id`, `slot_name`, `block_num`) to SMT root.
    /// Populated during block import for all storage map slots.
    storage_roots: BTreeMap<(AccountId, StorageSlotName, BlockNumber), Word>,

    /// Maps (`account_id`, `block_num`) to vault SMT root.
    /// Tracks asset vault versions across all blocks with structural sharing.
    vault_roots: BTreeMap<(AccountId, BlockNumber), Word>,
}

impl InnerForest {
    pub(crate) fn new() -> Self {
        Self {
            storage_forest: SmtForest::new(),
            storage_roots: BTreeMap::new(),
            vault_roots: BTreeMap::new(),
        }
    }

    // HELPERS
    // --------------------------------------------------------------------------------------------

    /// Returns the root of an empty SMT.
    fn empty_smt_root() -> Word {
        *EmptySubtreeRoots::entry(SMT_DEPTH, 0)
    }

    /// Retrieves the vault SMT root for an account at a given block, defaulting to empty.
    fn get_vault_root(&self, account_id: AccountId, block_num: BlockNumber) -> Word {
        self.vault_roots
            .get(&(account_id, block_num))
            .copied()
            .unwrap_or_else(Self::empty_smt_root)
    }

    /// Retrieves the storage map SMT root for an account slot at a given block, defaulting to
    /// empty.
    fn get_storage_root(
        &self,
        account_id: AccountId,
        slot_name: &StorageSlotName,
        block_num: BlockNumber,
    ) -> Word {
        self.storage_roots
            .get(&(account_id, slot_name.clone(), block_num))
            .copied()
            .unwrap_or_else(Self::empty_smt_root)
    }

    // PUBLIC INTERFACE
    // --------------------------------------------------------------------------------------------

    /// Updates the forest with account vault and storage changes from a delta.
    ///
    /// Unified interface for updating all account state in the forest, handling both
    /// full-state deltas (new accounts or reconstruction from DB) and partial deltas
    /// (incremental updates during block application).
    ///
    /// Full-state deltas (`delta.is_full_state() == true`) populate the forest from
    /// scratch using an empty SMT root. Partial deltas apply changes on top of the
    /// previous block's state.
    pub(crate) fn update_account(&mut self, block_num: BlockNumber, delta: &AccountDelta) {
        let account_id = delta.id();
        let is_full_state = delta.is_full_state();

        if !delta.vault().is_empty() {
            self.update_account_vault(block_num, account_id, delta.vault(), is_full_state);
        }

        if !delta.storage().is_empty() {
            self.update_account_storage(block_num, account_id, delta.storage(), is_full_state);
        }
    }

    // PRIVATE METHODS
    // --------------------------------------------------------------------------------------------

    /// Updates the forest with vault changes from a delta.
    ///
    /// Processes both fungible and non-fungible asset changes, building entries
    /// for the vault SMT and tracking the new root.
    fn update_account_vault(
        &mut self,
        block_num: BlockNumber,
        account_id: AccountId,
        vault_delta: &AccountVaultDelta,
        is_full_state: bool,
    ) {
        let prev_root = if is_full_state {
            Self::empty_smt_root()
        } else {
            self.get_vault_root(account_id, block_num.parent().unwrap_or_default())
        };

        let mut entries = Vec::new();

        // Process fungible assets
        for (faucet_id, amount) in vault_delta.fungible().iter() {
            let amount_u64: u64 = (*amount).try_into().expect("amount is non-negative");
            let asset: Asset =
                FungibleAsset::new(*faucet_id, amount_u64).expect("valid fungible asset").into();
            entries.push((asset.vault_key().into(), Word::from(asset)));
        }

        // Process non-fungible assets
        for (asset, action) in vault_delta.non_fungible().iter() {
            let value = match action {
                NonFungibleDeltaAction::Add => Word::from(Asset::NonFungible(*asset)),
                NonFungibleDeltaAction::Remove => EMPTY_WORD,
            };
            entries.push((asset.vault_key().into(), value));
        }

        if entries.is_empty() {
            return;
        }

        let updated_root = self
            .storage_forest
            .batch_insert(prev_root, entries.iter().copied())
            .expect("forest insertion should succeed");

        self.vault_roots.insert((account_id, block_num), updated_root);

        tracing::debug!(
            target: crate::COMPONENT,
            %account_id,
            %block_num,
            vault_entries = entries.len(),
            "Updated vault in forest"
        );
    }

    /// Updates the forest with storage map changes from a delta.
    ///
    /// Processes storage map slot deltas, building SMTs for each modified slot
    /// and tracking the new roots.
    fn update_account_storage(
        &mut self,
        block_num: BlockNumber,
        account_id: AccountId,
        storage_delta: &AccountStorageDelta,
        is_full_state: bool,
    ) {
        let parent_block = block_num.parent().unwrap_or_default();

        for (slot_name, map_delta) in storage_delta.maps() {
            let prev_root = if is_full_state {
                Self::empty_smt_root()
            } else {
                self.get_storage_root(account_id, slot_name, parent_block)
            };

            let entries: Vec<_> =
                map_delta.entries().iter().map(|(key, value)| ((*key).into(), *value)).collect();

            if entries.is_empty() {
                continue;
            }

            let updated_root = self
                .storage_forest
                .batch_insert(prev_root, entries.iter().copied())
                .expect("forest insertion should succeed");

            self.storage_roots
                .insert((account_id, slot_name.clone(), block_num), updated_root);

            tracing::debug!(
                target: crate::COMPONENT,
                %account_id,
                %block_num,
                ?slot_name,
                entries = entries.len(),
                "Updated storage map in forest"
            );
        }
    }
}
