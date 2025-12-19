use miden_objects::asset::{Asset, FungibleAsset};
use miden_objects::testing::account_id::{
    ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET,
    ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_IMMUTABLE_CODE,
};

use super::*;

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
fn test_update_vault_with_fungible_asset() {
    let mut forest = InnerForest::new();
    let account_id = test_account();
    let faucet_id = test_faucet();
    let block_num = BlockNumber::GENESIS.child();

    let asset = create_fungible_asset(faucet_id, 100);
    let vault_entries = vec![(asset.vault_key().into(), Word::from(asset))];

    forest.add_vault(account_id, &vault_entries, block_num);

    let vault_root = forest.vault_roots[&(account_id, block_num)];
    assert_ne!(vault_root, EMPTY_WORD);
}

#[test]
fn test_compare_delta_vs_db_vault_with_fungible_asset() {
    let account_id = test_account();
    let faucet_id = test_faucet();
    let block_num = BlockNumber::GENESIS.child();
    let asset = create_fungible_asset(faucet_id, 100);

    // DB-based population approach
    let mut forest_db = InnerForest::new();
    let vault_entries = vec![(asset.vault_key().into(), Word::from(asset))];
    forest_db.add_vault(account_id, &vault_entries, block_num);

    // Verify the root is set correctly
    let root_db = forest_db.vault_roots.get(&(account_id, block_num)).unwrap();
    assert_ne!(*root_db, EMPTY_WORD);
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

    // Block 1: 100 tokens
    let block_1 = BlockNumber::GENESIS.child();
    let asset_1 = create_fungible_asset(faucet_id, 100);
    let vault_entries_1 = vec![(asset_1.vault_key().into(), Word::from(asset_1))];
    forest.add_vault(account_id, &vault_entries_1, block_1);
    let root_1 = forest.vault_roots[&(account_id, block_1)];

    // Block 2: 150 tokens
    let block_2 = block_1.child();
    let asset_2 = create_fungible_asset(faucet_id, 150);
    let vault_entries_2 = vec![(asset_2.vault_key().into(), Word::from(asset_2))];
    forest.add_vault(account_id, &vault_entries_2, block_2);
    let root_2 = forest.vault_roots[&(account_id, block_2)];

    assert_ne!(root_1, root_2);
}
