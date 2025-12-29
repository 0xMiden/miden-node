use miden_protocol::account::AccountCode;
use miden_protocol::asset::{Asset, AssetVault, FungibleAsset};
use miden_protocol::testing::account_id::{
    ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET,
    ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_IMMUTABLE_CODE,
};
use miden_protocol::{Felt, FieldElement};

use super::*;

fn dummy_account() -> AccountId {
    AccountId::try_from(ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_IMMUTABLE_CODE).unwrap()
}

fn dummy_faucet() -> AccountId {
    AccountId::try_from(ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET).unwrap()
}

fn dummy_fungible_asset(faucet_id: AccountId, amount: u64) -> Asset {
    FungibleAsset::new(faucet_id, amount).unwrap().into()
}

/// Creates a partial `AccountDelta` (without code) for testing incremental updates.
fn dummy_partial_delta(
    account_id: AccountId,
    vault_delta: AccountVaultDelta,
    storage_delta: AccountStorageDelta,
) -> AccountDelta {
    // For partial deltas, nonce_delta must be > 0 if there are changes
    let nonce_delta = if vault_delta.is_empty() && storage_delta.is_empty() {
        Felt::ZERO
    } else {
        Felt::ONE
    };
    AccountDelta::new(account_id, storage_delta, vault_delta, nonce_delta).unwrap()
}

/// Creates a full-state `AccountDelta` (with code) for testing DB reconstruction.
fn dummy_full_state_delta(account_id: AccountId, assets: &[Asset]) -> AccountDelta {
    use miden_protocol::account::{Account, AccountStorage};

    // Create a minimal account with the given assets
    let vault = AssetVault::new(assets).unwrap();
    let storage = AccountStorage::new(vec![]).unwrap();
    let code = AccountCode::mock();
    let nonce = Felt::ONE;

    let account = Account::new(account_id, vault, storage, code, nonce, None).unwrap();

    // Convert to delta - this will be a full-state delta because it has code
    AccountDelta::try_from(account).unwrap()
}

#[test]
fn test_empty_smt_root_is_recognized() {
    use miden_protocol::crypto::merkle::smt::Smt;

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
    let account_id = dummy_account();
    let block_num = BlockNumber::GENESIS.child();

    let delta = dummy_partial_delta(
        account_id,
        AccountVaultDelta::default(),
        AccountStorageDelta::default(),
    );

    forest.update_account(block_num, &delta);

    // Empty deltas should not create entries
    assert!(!forest.vault_roots.contains_key(&(account_id, block_num)));
    assert!(forest.storage_roots.is_empty());
}

#[test]
fn test_update_vault_with_fungible_asset() {
    let mut forest = InnerForest::new();
    let account_id = dummy_account();
    let faucet_id = dummy_faucet();
    let block_num = BlockNumber::GENESIS.child();

    let asset = dummy_fungible_asset(faucet_id, 100);
    let mut vault_delta = AccountVaultDelta::default();
    vault_delta.add_asset(asset).unwrap();

    let delta = dummy_partial_delta(account_id, vault_delta, AccountStorageDelta::default());
    forest.update_account(block_num, &delta);

    let vault_root = forest.vault_roots[&(account_id, block_num)];
    assert_ne!(vault_root, EMPTY_WORD);
}

#[test]
fn test_compare_partial_vs_full_state_delta_vault() {
    let account_id = dummy_account();
    let faucet_id = dummy_faucet();
    let block_num = BlockNumber::GENESIS.child();
    let asset = dummy_fungible_asset(faucet_id, 100);

    // Approach 1: Partial delta (simulates block application)
    let mut forest_partial = InnerForest::new();
    let mut vault_delta = AccountVaultDelta::default();
    vault_delta.add_asset(asset).unwrap();
    let partial_delta =
        dummy_partial_delta(account_id, vault_delta, AccountStorageDelta::default());
    forest_partial.update_account(block_num, &partial_delta);

    // Approach 2: Full-state delta (simulates DB reconstruction)
    let mut forest_full = InnerForest::new();
    let full_delta = dummy_full_state_delta(account_id, &[asset]);
    forest_full.update_account(block_num, &full_delta);

    // Both approaches must produce identical vault roots
    let root_partial = forest_partial.vault_roots.get(&(account_id, block_num)).unwrap();
    let root_full = forest_full.vault_roots.get(&(account_id, block_num)).unwrap();

    assert_eq!(root_partial, root_full);
    assert_ne!(*root_partial, EMPTY_WORD);
}

#[test]
fn test_slot_names_are_tracked() {
    let forest = InnerForest::new();
    let _: &BTreeMap<(AccountId, StorageSlotName, BlockNumber), Word> = &forest.storage_roots;
}

#[test]
fn test_incremental_vault_updates() {
    let mut forest = InnerForest::new();
    let account_id = dummy_account();
    let faucet_id = dummy_faucet();

    // Block 1: 100 tokens
    let block_1 = BlockNumber::GENESIS.child();
    let mut vault_delta_1 = AccountVaultDelta::default();
    vault_delta_1.add_asset(dummy_fungible_asset(faucet_id, 100)).unwrap();
    let delta_1 = dummy_partial_delta(account_id, vault_delta_1, AccountStorageDelta::default());
    forest.update_account(block_1, &delta_1);
    let root_1 = forest.vault_roots[&(account_id, block_1)];

    // Block 2: 150 tokens (update)
    let block_2 = block_1.child();
    let mut vault_delta_2 = AccountVaultDelta::default();
    vault_delta_2.add_asset(dummy_fungible_asset(faucet_id, 150)).unwrap();
    let delta_2 = dummy_partial_delta(account_id, vault_delta_2, AccountStorageDelta::default());
    forest.update_account(block_2, &delta_2);
    let root_2 = forest.vault_roots[&(account_id, block_2)];

    assert_ne!(root_1, root_2);
}

#[test]
fn test_full_state_delta_starts_from_empty_root() {
    let mut forest = InnerForest::new();
    let account_id = dummy_account();
    let faucet_id = dummy_faucet();
    let block_num = BlockNumber::GENESIS.child();

    // Simulate a pre-existing vault state that should be ignored for full-state deltas
    let mut vault_delta_pre = AccountVaultDelta::default();
    vault_delta_pre.add_asset(dummy_fungible_asset(faucet_id, 999)).unwrap();
    let delta_pre =
        dummy_partial_delta(account_id, vault_delta_pre, AccountStorageDelta::default());
    forest.update_account(block_num, &delta_pre);
    assert!(forest.vault_roots.contains_key(&(account_id, block_num)));

    // Now create a full-state delta at the same block
    // A full-state delta should start from an empty root, not from the previous state
    let asset = dummy_fungible_asset(faucet_id, 100);
    let full_delta = dummy_full_state_delta(account_id, &[asset]);

    // Create a fresh forest to compare
    let mut fresh_forest = InnerForest::new();
    fresh_forest.update_account(block_num, &full_delta);
    let fresh_root = fresh_forest.vault_roots[&(account_id, block_num)];

    // Update the original forest with the full-state delta
    forest.update_account(block_num, &full_delta);
    let updated_root = forest.vault_roots[&(account_id, block_num)];

    // The full-state delta should produce the same root regardless of prior state
    assert_eq!(updated_root, fresh_root);
}

#[test]
fn test_vault_state_persists_across_blocks_without_changes() {
    // Regression test for issue #7: vault state should persist across blocks
    // where no changes occur, not reset to empty.
    let mut forest = InnerForest::new();
    let account_id = dummy_account();
    let faucet_id = dummy_faucet();

    // Block 1: Add 100 tokens
    let block_1 = BlockNumber::GENESIS.child();
    let mut vault_delta_1 = AccountVaultDelta::default();
    vault_delta_1.add_asset(dummy_fungible_asset(faucet_id, 100)).unwrap();
    let delta_1 = dummy_partial_delta(account_id, vault_delta_1, AccountStorageDelta::default());
    forest.update_account(block_1, &delta_1);
    let root_after_block_1 = forest.vault_roots[&(account_id, block_1)];

    // Blocks 2-5: No changes to this account (simulated by not calling update_account)
    // This means no entries are added to vault_roots for these blocks.

    // Block 6: Add 50 more tokens
    // The previous root lookup should find block_1's root, not return empty.
    let block_6 = BlockNumber::from(6);
    let mut vault_delta_6 = AccountVaultDelta::default();
    vault_delta_6.add_asset(dummy_fungible_asset(faucet_id, 150)).unwrap(); // 100 + 50 = 150
    let delta_6 = dummy_partial_delta(account_id, vault_delta_6, AccountStorageDelta::default());
    forest.update_account(block_6, &delta_6);

    // The root at block 6 should be different from block 1 (we added more tokens)
    let root_after_block_6 = forest.vault_roots[&(account_id, block_6)];
    assert_ne!(root_after_block_1, root_after_block_6);

    // Verify get_vault_root finds the correct previous root for intermediate blocks
    // Block 3 should return block 1's root (most recent before block 3)
    let root_at_block_3 = forest.get_vault_root(account_id, BlockNumber::from(3));
    assert_eq!(root_at_block_3, root_after_block_1);

    // Block 5 should also return block 1's root
    let root_at_block_5 = forest.get_vault_root(account_id, BlockNumber::from(5));
    assert_eq!(root_at_block_5, root_after_block_1);

    // Block 6 should return block 6's root
    let root_at_block_6 = forest.get_vault_root(account_id, block_6);
    assert_eq!(root_at_block_6, root_after_block_6);
}
