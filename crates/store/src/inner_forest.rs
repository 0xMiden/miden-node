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

    /// Returns the root of an empty SMT.
    fn empty_smt_root() -> Word {
        *EmptySubtreeRoots::entry(SMT_DEPTH, 0)
    }

    /// Updates the forest with account vault and storage changes from a delta.
    ///
    /// This is the unified interface for updating all account state in the forest.
    /// It handles both full-state deltas (new accounts or reconstruction from DB)
    /// and partial deltas (incremental updates during block application).
    ///
    /// For full-state deltas (`delta.is_full_state() == true`), the forest is populated
    /// from scratch using an empty SMT root. For partial deltas, changes are applied
    /// on top of the previous block's state.
    ///
    /// # Arguments
    ///
    /// * `block_num` - Block number for which these changes are being applied
    /// * `delta` - The account delta containing vault and storage changes
    pub(crate) fn update_account(&mut self, block_num: BlockNumber, delta: &AccountDelta) {
        let account_id = delta.id();
        let is_full_state = delta.is_full_state();

        // Update vault if there are any changes
        if !delta.vault().is_empty() {
            self.update_account_vault(block_num, account_id, delta.vault(), is_full_state);
        }

        // Update storage maps if there are any changes
        if !delta.storage().is_empty() {
            self.update_account_storage(block_num, account_id, delta.storage(), is_full_state);
        }
    }

    /// Updates the forest with vault changes from a delta.
    ///
    /// Processes both fungible and non-fungible asset changes, building entries
    /// for the vault SMT and tracking the new root.
    ///
    /// # Arguments
    ///
    /// * `block_num` - Block number for this update
    /// * `account_id` - The account being updated
    /// * `vault_delta` - Changes to the account's asset vault
    /// * `is_full_state` - If true, start from empty root; otherwise use previous block's root
    fn update_account_vault(
        &mut self,
        block_num: BlockNumber,
        account_id: AccountId,
        vault_delta: &AccountVaultDelta,
        is_full_state: bool,
    ) {
        // For full-state deltas (new accounts or reconstruction), start from empty root.
        // For partial deltas, look up the previous block's root.
        let prev_root = if is_full_state {
            Self::empty_smt_root()
        } else {
            let prev_block_num = block_num.parent().unwrap_or_default();
            self.vault_roots
                .get(&(account_id, prev_block_num))
                .copied()
                .unwrap_or_else(Self::empty_smt_root)
        };

        // Collect all vault entry updates
        let mut entries = Vec::new();

        // Process fungible assets - these require special handling to get current amounts
        // Note: We rely on the delta containing the updated amounts, not just the changes
        for (faucet_id, amount) in vault_delta.fungible().iter() {
            let amount_u64 = (*amount).try_into().expect("Amount should be non-negative");
            let asset: Asset = FungibleAsset::new(*faucet_id, amount_u64)
                .expect("Valid fungible asset from delta")
                .into();
            entries.push((asset.vault_key().into(), Word::from(asset)));
        }

        // Process non-fungible assets
        for (asset, action) in vault_delta.non_fungible().iter() {
            match action {
                NonFungibleDeltaAction::Add => {
                    entries
                        .push((asset.vault_key().into(), Word::from(Asset::NonFungible(*asset))));
                },
                NonFungibleDeltaAction::Remove => {
                    entries.push((asset.vault_key().into(), EMPTY_WORD));
                },
            }
        }

        if !entries.is_empty() {
            let updated_root = self
                .storage_forest
                .batch_insert(prev_root, entries.iter().copied())
                .expect("Forest insertion should succeed");

            self.vault_roots.insert((account_id, block_num), updated_root);

            tracing::debug!(
                target: crate::COMPONENT,
                account_id = %account_id,
                block_num = %block_num,
                vault_entries = entries.len(),
                "Updated vault in forest"
            );
        }
    }

    /// Updates the forest with storage map changes from a delta.
    ///
    /// Processes storage map slot deltas, building SMTs for each modified slot
    /// and tracking the new roots.
    ///
    /// # Arguments
    ///
    /// * `block_num` - Block number for this update
    /// * `account_id` - The account being updated
    /// * `storage_delta` - Changes to the account's storage maps
    /// * `is_full_state` - If true, start from empty root; otherwise use previous block's root
    fn update_account_storage(
        &mut self,
        block_num: BlockNumber,
        account_id: AccountId,
        storage_delta: &AccountStorageDelta,
        is_full_state: bool,
    ) {
        for (slot_name, map_delta) in storage_delta.maps() {
            // For full-state deltas (new accounts or reconstruction), start from empty root.
            // For partial deltas, look up the previous block's root.
            let prev_root = if is_full_state {
                Self::empty_smt_root()
            } else {
                let prev_block_num = block_num.parent().unwrap_or_default();
                self.storage_roots
                    .get(&(account_id, slot_name.clone(), prev_block_num))
                    .copied()
                    .unwrap_or_else(Self::empty_smt_root)
            };

            // Collect entries from the delta
            let entries = map_delta
                .entries()
                .iter()
                .map(|(key, value)| ((*key).into(), *value))
                .collect::<Vec<_>>();

            if !entries.is_empty() {
                let updated_root = self
                    .storage_forest
                    .batch_insert(prev_root, entries.iter().copied())
                    .expect("Forest insertion should succeed");

                self.storage_roots
                    .insert((account_id, slot_name.clone(), block_num), updated_root);

                tracing::debug!(
                    target: crate::COMPONENT,
                    account_id = %account_id,
                    block_num = %block_num,
                    slot_name = ?slot_name,
                    entries = entries.len(),
                    "Updated storage map in forest"
                );
            }
        }
    }
}
