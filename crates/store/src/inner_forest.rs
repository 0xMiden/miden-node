use std::collections::BTreeMap;

use miden_objects::account::{AccountId, StorageSlotName};
use miden_objects::block::BlockNumber;
use miden_objects::crypto::merkle::{EmptySubtreeRoots, SMT_DEPTH, SmtForest};
use miden_objects::{EMPTY_WORD, Word};

use crate::errors::DatabaseError;

#[cfg(test)]
mod tests;

type MapSlotEntries = Vec<(Word, Word)>;

type VaultEntries = Vec<(Word, Word)>;

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

    /// Populates storage map SMTs in the forest from full database state for a single account.
    pub(crate) fn add_storage_map(
        &mut self,
        account_id: AccountId,
        map_slots_to_populate: Vec<(StorageSlotName, MapSlotEntries)>,
        block_num: BlockNumber,
    ) {
        for (slot_name, entries) in map_slots_to_populate {
            if entries.is_empty() {
                continue;
            }

            let updated_root = self
                .storage_forest
                .batch_insert(Self::empty_smt_root(), entries.iter().copied())
                .expect("Forest insertion should succeed");

            self.storage_roots
                .insert((account_id, slot_name.clone(), block_num), updated_root);

            tracing::debug!(
                target: crate::COMPONENT,
                account_id = %account_id,
                block_num = %block_num,
                slot_name = ?slot_name,
                entries = entries.len(),
                "Populated storage map in forest from DB"
            );
        }
    }

    /// Populates a vault SMT in the forest from full database state.
    pub(crate) fn add_vault(
        &mut self,
        account_id: AccountId,
        vault_entries: &VaultEntries,
        block_num: BlockNumber,
    ) {
        if vault_entries.is_empty() {
            return;
        }

        let updated_root = self
            .storage_forest
            .batch_insert(Self::empty_smt_root(), vault_entries.iter().copied())
            .expect("Forest insertion should succeed");

        self.vault_roots.insert((account_id, block_num), updated_root);

        tracing::debug!(
            target: crate::COMPONENT,
            account_id = %account_id,
            block_num = %block_num,
            vault_entries = vault_entries.len(),
            "Populated vault in forest from DB"
        );
    }

    /// Queries specific storage keys for a given account and slot at a specific block.
    ///
    /// Keys that don't exist in the storage map will have a value of `EMPTY_WORD`.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The storage root for this account/slot/block is not tracked
    /// - The forest doesn't have sufficient data to provide proofs for the keys
    pub(crate) fn query_storage_keys(
        &self,
        account_id: AccountId,
        slot_name: &StorageSlotName,
        block_num: BlockNumber,
        keys: &[Word],
    ) -> Result<Vec<(Word, Word)>, DatabaseError> {
        // Get the storage root for this account/slot/block
        let root = self
            .storage_roots
            .get(&(account_id, slot_name.clone(), block_num))
            .copied()
            .ok_or_else(|| DatabaseError::StorageRootNotFound {
                account_id,
                slot_name: slot_name.to_string(),
                block_num,
            })?;

        let mut results = Vec::with_capacity(keys.len());

        for key in keys {
            let proof = self.storage_forest.open(root, *key)?;
            let value = proof.get(key).unwrap_or(EMPTY_WORD);
            results.push((*key, value));
        }

        tracing::debug!(
            target: crate::COMPONENT,
            %account_id,
            %block_num,
            ?slot_name,
            num_keys = results.len(),
            "Queried storage keys from forest"
        );

        Ok(results)
    }
}
