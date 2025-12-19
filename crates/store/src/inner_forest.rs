use std::collections::BTreeMap;

use miden_objects::account::delta::{AccountStorageDelta, AccountVaultDelta};
use miden_objects::account::{AccountId, NonFungibleDeltaAction, StorageSlotName};
use miden_objects::asset::{Asset, FungibleAsset};
use miden_objects::block::BlockNumber;
use miden_objects::crypto::merkle::{EmptySubtreeRoots, SMT_DEPTH, SmtForest};
use miden_objects::{EMPTY_WORD, Word};

// Type aliases to reduce complexity
type MapSlotEntries = Vec<(Word, Word)>;
type StorageMapSlot = (AccountId, StorageSlotName, MapSlotEntries);
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

    /// Updates the forest with account vault and storage changes from a delta.
    ///
    /// This is the unified interface for updating all account state in the forest.
    /// It processes both vault and storage map deltas and updates the forest accordingly.
    ///
    /// # Arguments
    ///
    /// * `block_num` - Block number for which these changes are being applied
    /// * `account_id` - The account being updated
    /// * `vault_delta` - Changes to the account's asset vault
    /// * `storage_delta` - Changes to the account's storage maps
    pub(crate) fn update_account(
        &mut self,
        block_num: BlockNumber,
        account_id: AccountId,
        vault_delta: &AccountVaultDelta,
        storage_delta: &AccountStorageDelta,
    ) {
        // Update vault if there are any changes
        if !vault_delta.is_empty() {
            self.update_account_vault(block_num, account_id, vault_delta);
        }

        // Update storage maps if there are any changes
        if !storage_delta.is_empty() {
            self.update_account_storage(block_num, account_id, storage_delta);
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
    fn update_account_vault(
        &mut self,
        block_num: BlockNumber,
        account_id: AccountId,
        vault_delta: &AccountVaultDelta,
    ) {
        let prev_block_num = block_num.parent().unwrap_or_default();
        let prev_root = self
            .vault_roots
            .get(&(account_id, prev_block_num))
            .copied()
            .unwrap_or_else(Self::empty_smt_root);

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
    fn update_account_storage(
        &mut self,
        block_num: BlockNumber,
        account_id: AccountId,
        storage_delta: &AccountStorageDelta,
    ) {
        let prev_block_num = block_num.parent().unwrap_or_default();

        for (slot_name, map_delta) in storage_delta.maps() {
            let prev_root = self
                .storage_roots
                .get(&(account_id, slot_name.clone(), prev_block_num))
                .copied()
                .unwrap_or_else(Self::empty_smt_root);

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

    // LEGACY DB-BASED POPULATION METHODS
    // ================================================================================================
    // These methods are used during initial State::load() where deltas are not available.
    // They populate the forest from full database state rather than incremental deltas.
    //
    // For block application, prefer `update_account()` which uses deltas directly.

    /// Populates storage map SMTs in the forest from full database state.
    ///
    /// **DEPRECATED for block application**: Use `update_account()` with deltas instead.
    /// This method is primarily used during `State::load()` where deltas are not available.
    ///
    /// # Arguments
    ///
    /// * `map_slots_to_populate` - List of (`account_id`, `slot_name`, entries) tuples
    /// * `block_num` - Block number for which this state applies
    #[allow(dead_code)] // Used only during State::load
    pub(crate) fn populate_storage_maps(
        &mut self,
        map_slots_to_populate: Vec<StorageMapSlot>,
        block_num: BlockNumber,
    ) {
        for (account_id, slot_name, entries) in map_slots_to_populate {
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

    /// Populates vault SMTs in the forest from full database state.
    ///
    /// **DEPRECATED for block application**: Use `update_account()` with deltas instead.
    /// This method is primarily used during `State::load()` where deltas are not available.
    ///
    /// # Arguments
    ///
    /// * `vault_entries_to_populate` - List of (`account_id`, `vault_entries`) tuples where entries
    ///   are (key, value) Word pairs
    /// * `block_num` - Block number for which this state applies
    #[allow(dead_code)] // Used only during State::load
    pub(crate) fn populate_vaults(
        &mut self,
        vault_entries_to_populate: Vec<(AccountId, VaultEntries)>,
        block_num: BlockNumber,
    ) {
        for (account_id, entries) in vault_entries_to_populate {
            if entries.is_empty() {
                continue;
            }

            let updated_root = self
                .storage_forest
                .batch_insert(Self::empty_smt_root(), entries.iter().copied())
                .expect("Forest insertion should succeed");

            self.vault_roots.insert((account_id, block_num), updated_root);

            tracing::debug!(
                target: crate::COMPONENT,
                account_id = %account_id,
                block_num = %block_num,
                vault_entries = entries.len(),
                "Populated vault in forest from DB"
            );
        }
    }

    /// Helper method to extract storage map slots from `AccountStorage` objects.
    ///
    /// Used by the legacy DB-based population path during `State::load()`.
    ///
    /// # Returns
    ///
    /// Vector of (`account_id`, `slot_name`, entries) tuples ready for forest population
    pub(crate) fn extract_map_slots_from_storage(
        account_storages: &[(AccountId, miden_objects::account::AccountStorage)],
    ) -> Vec<StorageMapSlot> {
        use miden_objects::account::StorageSlotContent;

        let mut map_slots = Vec::new();

        for (account_id, storage) in account_storages {
            for slot in storage.slots() {
                if let StorageSlotContent::Map(map) = slot.content() {
                    let entries: Vec<_> = map.entries().map(|(k, v)| (*k, *v)).collect();

                    if !entries.is_empty() {
                        map_slots.push((*account_id, slot.name().clone(), entries));
                    }
                }
            }
        }

        map_slots
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miden_objects::asset::{Asset, FungibleAsset};
    use miden_objects::testing::account_id::{
        ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET,
        ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_IMMUTABLE_CODE,
    };

    fn test_account() -> AccountId {
        AccountId::try_from(ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_IMMUTABLE_CODE).unwrap()
    }

    fn test_faucet() -> AccountId {
        AccountId::try_from(ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET).unwrap()
    }

    fn create_fungible_asset(faucet_id: AccountId, amount: u64) -> Asset {
        FungibleAsset::new(faucet_id, amount).unwrap().into()
    }

    #[test]
    fn test_empty_smt_root_is_recognized() {
        use miden_objects::crypto::merkle::Smt;

        let empty_root = InnerForest::empty_smt_root();

        // Verify an empty SMT has the expected root
        assert_eq!(Smt::default().root(), empty_root);

        // Test that SmtForest accepts this root in batch_insert
        let mut forest = SmtForest::new();
        let entries = vec![(Word::from([1u32, 2, 3, 4]), Word::from([5u32, 6, 7, 8]))];

        assert!(forest.batch_insert(empty_root, entries).is_ok());
    }

    #[test]
    fn test_inner_forest_basic_initialization() {
        let forest = InnerForest::new();
        assert!(forest.storage_roots.is_empty());
        assert!(forest.vault_roots.is_empty());
    }

    #[test]
    fn test_update_account_with_empty_deltas() {
        let mut forest = InnerForest::new();
        let account_id = test_account();
        let block_num = BlockNumber::GENESIS.child();

        let vault_delta = AccountVaultDelta::default();
        let storage_delta = AccountStorageDelta::default();

        forest.update_account(block_num, account_id, &vault_delta, &storage_delta);

        // Empty deltas should not create entries
        assert!(!forest.vault_roots.contains_key(&(account_id, block_num)));
        assert!(forest.storage_roots.is_empty());
    }

    #[test]
    fn test_update_vault_with_fungible_asset() {
        let mut forest = InnerForest::new();
        let account_id = test_account();
        let faucet_id = test_faucet();
        let block_num = BlockNumber::GENESIS.child();

        let asset = create_fungible_asset(faucet_id, 100);
        let mut vault_delta = AccountVaultDelta::default();
        vault_delta.add_asset(asset).unwrap();

        forest.update_account(block_num, account_id, &vault_delta, &AccountStorageDelta::default());

        let vault_root = forest.vault_roots[&(account_id, block_num)];
        assert_ne!(vault_root, EMPTY_WORD);
    }

    #[test]
    fn test_compare_delta_vs_db_vault_with_fungible_asset() {
        let account_id = test_account();
        let faucet_id = test_faucet();
        let block_num = BlockNumber::GENESIS.child();
        let asset = create_fungible_asset(faucet_id, 100);

        // Approach 1: Delta-based update
        let mut forest_delta = InnerForest::new();
        let mut vault_delta = AccountVaultDelta::default();
        vault_delta.add_asset(asset).unwrap();
        forest_delta.update_account(
            block_num,
            account_id,
            &vault_delta,
            &AccountStorageDelta::default(),
        );

        // Approach 2: DB-based population
        let mut forest_db = InnerForest::new();
        let vault_entries = vec![(asset.vault_key().into(), Word::from(asset))];
        forest_db.populate_vaults(vec![(account_id, vault_entries)], block_num);

        // Both approaches must produce identical roots
        let root_delta = forest_delta.vault_roots.get(&(account_id, block_num)).unwrap();
        let root_db = forest_db.vault_roots.get(&(account_id, block_num)).unwrap();

        assert_eq!(root_delta, root_db);
        assert_ne!(*root_delta, EMPTY_WORD);
    }

    #[test]
    fn test_slot_names_are_tracked() {
        let forest = InnerForest::new();
        let _: &BTreeMap<(AccountId, StorageSlotName, BlockNumber), Word> = &forest.storage_roots;
    }

    #[test]
    fn test_incremental_vault_updates() {
        let mut forest = InnerForest::new();
        let account_id = test_account();
        let faucet_id = test_faucet();
        let storage_delta = AccountStorageDelta::default();

        // Block 1: 100 tokens
        let block_1 = BlockNumber::GENESIS.child();
        let mut vault_delta_1 = AccountVaultDelta::default();
        vault_delta_1.add_asset(create_fungible_asset(faucet_id, 100)).unwrap();
        forest.update_account(block_1, account_id, &vault_delta_1, &storage_delta);
        let root_1 = forest.vault_roots[&(account_id, block_1)];

        // Block 2: 150 tokens
        let block_2 = block_1.child();
        let mut vault_delta_2 = AccountVaultDelta::default();
        vault_delta_2.add_asset(create_fungible_asset(faucet_id, 150)).unwrap();
        forest.update_account(block_2, account_id, &vault_delta_2, &storage_delta);
        let root_2 = forest.vault_roots[&(account_id, block_2)];

        assert_ne!(root_1, root_2);
    }
}
