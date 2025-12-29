use std::collections::BTreeMap;

use miden_objects::account::delta::{AccountDelta, AccountStorageDelta, AccountVaultDelta};
use miden_objects::account::{AccountId, NonFungibleDeltaAction, StorageSlotName};
use miden_objects::asset::{Asset, FungibleAsset};
use miden_objects::block::BlockNumber;
use miden_objects::crypto::merkle::{EmptySubtreeRoots, SMT_DEPTH, SmtForest};
use miden_objects::{EMPTY_WORD, Word};

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

    /// Maps (`account_id`, `slot_name`, `block_num`) to all key-value entries in that storage map.
    /// Accumulated from deltas - each block's entries include all entries up to that point.
    storage_entries: BTreeMap<(AccountId, StorageSlotName, BlockNumber), BTreeMap<Word, Word>>,

    /// Maps (`account_id`, `block_num`) to vault SMT root.
    /// Tracks asset vault versions across all blocks with structural sharing.
    vault_roots: BTreeMap<(AccountId, BlockNumber), Word>,
}

impl InnerForest {
    pub(crate) fn new() -> Self {
        Self {
            storage_forest: SmtForest::new(),
            storage_roots: BTreeMap::new(),
            storage_entries: BTreeMap::new(),
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

    /// Returns the storage forest and the root for a specific account storage slot at a block.
    ///
    /// This allows callers to query specific keys from the storage map using `SmtForest::open()`.
    /// Returns `None` if no storage root is tracked for this account/slot/block combination.
    pub(crate) fn storage_map_forest_with_root(
        &self,
        account_id: AccountId,
        slot_name: &StorageSlotName,
        block_num: BlockNumber,
    ) -> Option<(&SmtForest, Word)> {
        let root = self.storage_roots.get(&(account_id, slot_name.clone(), block_num))?;
        Some((&self.storage_forest, *root))
    }

    /// Returns all key-value entries for a specific account storage slot at a block.
    ///
    /// Returns `None` if no entries are tracked for this account/slot/block combination.
    pub(crate) fn storage_map_entries(
        &self,
        account_id: AccountId,
        slot_name: &StorageSlotName,
        block_num: BlockNumber,
    ) -> Option<Vec<(Word, Word)>> {
        let entries = self.storage_entries.get(&(account_id, slot_name.clone(), block_num))?;
        Some(entries.iter().map(|(k, v)| (*k, *v)).collect())
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
    /// and tracking the new roots and accumulated entries.
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

            let delta_entries: Vec<_> =
                map_delta.entries().iter().map(|(key, value)| ((*key).into(), *value)).collect();

            if delta_entries.is_empty() {
                continue;
            }

            let updated_root = self
                .storage_forest
                .batch_insert(prev_root, delta_entries.iter().copied())
                .expect("forest insertion should succeed");

            self.storage_roots
                .insert((account_id, slot_name.clone(), block_num), updated_root);

            // Accumulate entries: start from parent block's entries or empty for full state
            let mut accumulated_entries = if is_full_state {
                BTreeMap::new()
            } else {
                self.storage_entries
                    .get(&(account_id, slot_name.clone(), parent_block))
                    .cloned()
                    .unwrap_or_default()
            };

            // Apply delta entries (insert or remove if value is EMPTY_WORD)
            for (key, value) in delta_entries.iter() {
                if *value == EMPTY_WORD {
                    accumulated_entries.remove(key);
                } else {
                    accumulated_entries.insert(*key, *value);
                }
            }

            self.storage_entries
                .insert((account_id, slot_name.clone(), block_num), accumulated_entries);

            tracing::debug!(
                target: crate::COMPONENT,
                %account_id,
                %block_num,
                ?slot_name,
                delta_entries = delta_entries.len(),
                "Updated storage map in forest"
            );
        }
    }
}
