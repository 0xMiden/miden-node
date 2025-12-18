use std::collections::BTreeMap;

use miden_objects::account::{AccountId, AccountStorage, StorageSlotContent};
use miden_objects::block::BlockNumber;
use miden_objects::crypto::merkle::SmtForest;
use miden_objects::{EMPTY_WORD, Word};

/// Container for forest-related state that needs to be updated atomically.
pub(crate) struct InnerForest {
    /// `SmtForest` for efficient account storage reconstruction.
    /// Populated during block import with storage and vault SMTs.
    pub(crate) storage_forest: SmtForest,

    /// Maps (`account_id`, `slot_index`, `block_num`) to SMT root.
    /// Populated during block import for all storage map slots.
    storage_roots: BTreeMap<(AccountId, u8, BlockNumber), Word>,

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

    /// Extracts map-type storage slots and their entries from account storage data.
    ///
    /// This is a helper method to prepare data for populating the forest with storage maps.
    /// It iterates through all accounts' storage slots and collects only the map-type slots
    /// with their entries.
    ///
    /// # Arguments
    ///
    /// * `account_storages` - Slice of `(account_id, storage)` tuples from database
    ///
    /// # Returns
    ///
    /// Vec of `(account_id, slot_index, entries)` tuples ready for forest population
    #[allow(clippy::type_complexity)]
    pub(crate) fn extract_map_slots_from_storage(
        account_storages: &[(AccountId, AccountStorage)],
    ) -> Vec<(AccountId, u8, Vec<(&Word, &Word)>)> {
        let mut map_slots = Vec::new();

        for (account_id, storage) in account_storages {
            for (slot_idx, slot) in storage.slots().iter().enumerate() {
                if let StorageSlotContent::Map(storage_map) = slot.content() {
                    let entries = Vec::from_iter(storage_map.entries());
                    map_slots.push((*account_id, slot_idx as u8, entries));
                }
            }
        }

        tracing::debug!(target: crate::COMPONENT, num_map_slots = map_slots.len());
        map_slots
    }

    /// Populates the forest with storage map SMTs for the given slots.
    ///
    /// This method builds SMTs from the provided entries and tracks their roots,
    /// enabling efficient historical queries with structural sharing.
    ///
    /// # Arguments
    ///
    /// * `map_slots` - Vec of `(account_id, slot_index, entries)` tuples
    /// * `block_num` - Block number for which these SMTs are being created
    #[allow(clippy::type_complexity)]
    pub(crate) fn populate_storage_maps(
        &mut self,
        map_slots: Vec<(AccountId, u8, Vec<(&Word, &Word)>)>,
        block_num: BlockNumber,
    ) {
        let prev_block_num = block_num.parent().unwrap_or_default();

        for (account_id, slot_idx, entries) in map_slots {
            // Get previous root for structural sharing
            let prev_root = self
                .storage_roots
                .get(&(account_id, slot_idx, prev_block_num))
                .copied()
                .unwrap_or(EMPTY_WORD);

            // Build new SMT from entries
            let updated_root = self
                .storage_forest
                .batch_insert(prev_root, entries.into_iter().map(|(k, v)| (*k, *v)))
                .expect("Forest insertion should always succeed with valid entries");

            // Track the new root
            self.storage_roots.insert((account_id, slot_idx, block_num), updated_root);
        }

        tracing::debug!(
            target: crate::COMPONENT,
            total_tracked_roots = self.storage_roots.len(),
            "Updated storage map roots"
        );
    }

    /// Populates the forest with vault SMTs for the given accounts.
    ///
    /// This method builds vault SMTs from the provided asset entries and tracks their roots,
    /// enabling efficient historical queries with structural sharing.
    ///
    /// # Arguments
    ///
    /// * `vault_entries` - Vec of `(account_id, entries)` tuples where entries are (key, value)
    ///   pairs
    /// * `block_num` - Block number for which these vault SMTs are being created
    pub(crate) fn populate_vaults(
        &mut self,
        vault_entries: Vec<(AccountId, Vec<(Word, Word)>)>,
        block_num: BlockNumber,
    ) {
        let prev_block_num = block_num.parent().unwrap_or_default();

        for (account_id, entries) in vault_entries {
            let prev_root = self
                .vault_roots
                .get(&(account_id, prev_block_num))
                .copied()
                .unwrap_or(EMPTY_WORD);

            let updated_root = self
                .storage_forest
                .batch_insert(prev_root, entries)
                .expect("Database is consistent and always allows constructing a smt or forest");

            // Track the new vault root
            self.vault_roots.insert((account_id, block_num), updated_root);
        }

        tracing::debug!(
            target: crate::COMPONENT,
            total_vault_roots = self.vault_roots.len(),
            "Updated vault roots"
        );
    }
}
