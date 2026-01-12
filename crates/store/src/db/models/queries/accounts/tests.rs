//! Tests for the `accounts` module, specifically for account storage and historical queries.

use diesel::query_dsl::methods::SelectDsl;
use diesel::{Connection, OptionalExtension, QueryDsl, RunQueryDsl};
use diesel_migrations::MigrationHarness;
use miden_node_utils::fee::test_fee_params;
use miden_protocol::account::auth::PublicKeyCommitment;
use miden_protocol::account::delta::AccountUpdateDetails;
use miden_protocol::account::{
    Account,
    AccountBuilder,
    AccountComponent,
    AccountDelta,
    AccountId,
    AccountIdVersion,
    AccountStorageMode,
    AccountType,
    StorageSlot,
    StorageSlotName,
};
use miden_protocol::block::{BlockAccountUpdate, BlockHeader, BlockNumber};
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SecretKey;
use miden_protocol::utils::Serializable;
use miden_protocol::{EMPTY_WORD, Felt, Word};
use miden_standards::account::auth::AuthRpoFalcon512;
use miden_standards::code_builder::CodeBuilder;

use super::*;
use crate::db::migrations::MIGRATIONS;

fn setup_test_db() -> SqliteConnection {
    let mut conn =
        SqliteConnection::establish(":memory:").expect("Failed to create in-memory database");

    conn.run_pending_migrations(MIGRATIONS).expect("Failed to run migrations");

    conn
}

fn create_test_account_with_storage() -> (Account, AccountId) {
    // Create a simple public account with one value storage slot
    let account_id = AccountId::dummy(
        [1u8; 15],
        AccountIdVersion::Version0,
        AccountType::RegularAccountImmutableCode,
        AccountStorageMode::Public,
    );

    let storage_value = Word::from([Felt::new(1), Felt::new(2), Felt::new(3), Felt::new(4)]);
    let component_storage = vec![StorageSlot::with_value(StorageSlotName::mock(0), storage_value)];

    let account_component_code = CodeBuilder::default()
        .compile_component_code("test::interface", "pub proc foo push.1 end")
        .unwrap();

    let component = AccountComponent::new(account_component_code, component_storage)
        .unwrap()
        .with_supported_type(AccountType::RegularAccountImmutableCode);

    let account = AccountBuilder::new([1u8; 32])
        .account_type(AccountType::RegularAccountImmutableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_component(component)
        .with_auth_component(AuthRpoFalcon512::new(PublicKeyCommitment::from(EMPTY_WORD)))
        .build_existing()
        .unwrap();

    (account, account_id)
}

fn insert_block_header(conn: &mut SqliteConnection, block_num: BlockNumber) {
    use crate::db::schema::block_headers;

    let block_header = BlockHeader::new(
        1_u8.into(),
        Word::default(),
        block_num,
        Word::default(),
        Word::default(),
        Word::default(),
        Word::default(),
        Word::default(),
        Word::default(),
        SecretKey::new().public_key(),
        test_fee_params(),
        0_u8.into(),
    );

    diesel::insert_into(block_headers::table)
        .values((
            block_headers::block_num.eq(i64::from(block_num.as_u32())),
            block_headers::block_header.eq(block_header.to_bytes()),
        ))
        .execute(conn)
        .expect("Failed to insert block header");
}

// ACCOUNT HEADER AT BLOCK TESTS
// ================================================================================================

#[test]
fn test_select_account_header_at_block_returns_none_for_nonexistent() {
    let mut conn = setup_test_db();
    let block_num = BlockNumber::from_epoch(0);
    insert_block_header(&mut conn, block_num);

    let account_id = AccountId::dummy(
        [99u8; 15],
        AccountIdVersion::Version0,
        AccountType::RegularAccountImmutableCode,
        AccountStorageMode::Public,
    );

    // Query for a non-existent account
    let result = select_account_header_at_block(&mut conn, account_id, block_num)
        .expect("Query should succeed");

    assert!(result.is_none(), "Should return None for non-existent account");
}

#[test]
fn test_select_account_header_at_block_returns_correct_header() {
    let mut conn = setup_test_db();
    let (account, _) = create_test_account_with_storage();
    let account_id = account.id();

    let block_num = BlockNumber::from_epoch(0);
    insert_block_header(&mut conn, block_num);

    // Insert the account
    let delta = AccountDelta::try_from(account.clone()).unwrap();
    let account_update = BlockAccountUpdate::new(
        account_id,
        account.commitment(),
        AccountUpdateDetails::Delta(delta),
    );

    upsert_accounts(&mut conn, &[account_update], block_num).expect("upsert_accounts failed");

    // Query the account header
    let (header, _storage_header) =
        select_account_header_at_block(&mut conn, account_id, block_num)
            .expect("Query should succeed")
            .expect("Header should exist");

    assert_eq!(header.id(), account_id, "Account ID should match");
    assert_eq!(header.nonce(), account.nonce(), "Nonce should match");
    assert_eq!(
        header.code_commitment(),
        account.code().commitment(),
        "Code commitment should match"
    );
}

#[test]
fn test_select_account_header_at_block_historical_query() {
    let mut conn = setup_test_db();
    let (account, _) = create_test_account_with_storage();
    let account_id = account.id();

    let block_num_1 = BlockNumber::from_epoch(0);
    let block_num_2 = BlockNumber::from_epoch(1);
    insert_block_header(&mut conn, block_num_1);
    insert_block_header(&mut conn, block_num_2);

    // Insert the account at block 1
    let nonce_1 = account.nonce();
    let delta_1 = AccountDelta::try_from(account.clone()).unwrap();
    let account_update_1 = BlockAccountUpdate::new(
        account_id,
        account.commitment(),
        AccountUpdateDetails::Delta(delta_1),
    );

    upsert_accounts(&mut conn, &[account_update_1], block_num_1).expect("First upsert failed");

    // Query at block 1 - should return the account
    let (header_1, _) = select_account_header_at_block(&mut conn, account_id, block_num_1)
        .expect("Query should succeed")
        .expect("Header should exist at block 1");

    assert_eq!(header_1.nonce(), nonce_1, "Nonce at block 1 should match");

    // Query at block 2 - should return the same account (most recent before block 2)
    let (header_2, _) = select_account_header_at_block(&mut conn, account_id, block_num_2)
        .expect("Query should succeed")
        .expect("Header should exist at block 2");

    assert_eq!(header_2.nonce(), nonce_1, "Nonce at block 2 should match block 1");
}

// ACCOUNT VAULT AT BLOCK TESTS
// ================================================================================================

#[test]
fn test_select_account_vault_at_block_empty() {
    let mut conn = setup_test_db();
    let (account, _) = create_test_account_with_storage();
    let account_id = account.id();

    let block_num = BlockNumber::from_epoch(0);
    insert_block_header(&mut conn, block_num);

    // Insert account without vault assets
    let delta = AccountDelta::try_from(account.clone()).unwrap();
    let account_update = BlockAccountUpdate::new(
        account_id,
        account.commitment(),
        AccountUpdateDetails::Delta(delta),
    );

    upsert_accounts(&mut conn, &[account_update], block_num).expect("upsert_accounts failed");

    // Query vault - should return empty (the test account has no assets)
    let assets = select_account_vault_at_block(&mut conn, account_id, block_num)
        .expect("Query should succeed");

    assert!(assets.is_empty(), "Account should have no assets");
}

// ACCOUNT STORAGE AT BLOCK TESTS
// ================================================================================================

#[test]
fn test_select_account_storage_at_block_returns_storage() {
    let mut conn = setup_test_db();
    let (account, _) = create_test_account_with_storage();
    let account_id = account.id();

    let block_num = BlockNumber::from_epoch(0);
    insert_block_header(&mut conn, block_num);

    let original_storage_commitment = account.storage().to_commitment();

    // Insert the account
    let delta = AccountDelta::try_from(account.clone()).unwrap();
    let account_update = BlockAccountUpdate::new(
        account_id,
        account.commitment(),
        AccountUpdateDetails::Delta(delta),
    );

    upsert_accounts(&mut conn, &[account_update], block_num).expect("upsert_accounts failed");

    // Query storage
    let storage = select_account_storage_at_block(&mut conn, account_id, block_num)
        .expect("Query should succeed");

    assert_eq!(
        storage.to_commitment(),
        original_storage_commitment,
        "Storage commitment should match"
    );
}

#[test]
fn test_upsert_accounts_inserts_storage_header() {
    let mut conn = setup_test_db();
    let (account, account_id) = create_test_account_with_storage();

    // Block 1
    let block_num = BlockNumber::from_epoch(0);
    insert_block_header(&mut conn, block_num);

    let storage_commitment_original = account.storage().to_commitment();
    let storage_slots_len = account.storage().slots().len();
    let account_commitment = account.commitment();

    // Create full state delta from the account
    let delta = AccountDelta::try_from(account).unwrap();
    assert!(delta.is_full_state(), "Delta should be full state");

    let account_update =
        BlockAccountUpdate::new(account_id, account_commitment, AccountUpdateDetails::Delta(delta));

    // Upsert account
    let result = upsert_accounts(&mut conn, &[account_update], block_num);
    assert!(result.is_ok(), "upsert_accounts failed: {:?}", result.err());
    assert_eq!(result.unwrap(), 1, "Expected 1 account to be inserted");

    // Query storage header back
    let queried_storage = select_latest_account_storage(&mut conn, account_id)
        .expect("Failed to query storage header");

    // Verify storage commitment matches
    assert_eq!(
        queried_storage.to_commitment(),
        storage_commitment_original,
        "Storage commitment mismatch"
    );

    // Verify number of slots matches
    assert_eq!(queried_storage.slots().len(), storage_slots_len, "Storage slots count mismatch");

    // Verify exactly 1 latest account with storage exists
    let header_count: i64 = schema::accounts::table
        .filter(schema::accounts::account_id.eq(account_id.to_bytes()))
        .filter(schema::accounts::is_latest.eq(true))
        .filter(schema::accounts::storage_header.is_not_null())
        .count()
        .get_result(&mut conn)
        .expect("Failed to count accounts with storage");

    assert_eq!(header_count, 1, "Expected exactly 1 latest account with storage");
}

#[test]
fn test_upsert_accounts_updates_is_latest_flag() {
    let mut conn = setup_test_db();
    let (account, account_id) = create_test_account_with_storage();

    // Block 1 and 2
    let block_num_1 = BlockNumber::from_epoch(0);
    let block_num_2 = BlockNumber::from_epoch(1);

    insert_block_header(&mut conn, block_num_1);
    insert_block_header(&mut conn, block_num_2);

    // Save storage commitment before moving account
    let storage_commitment_1 = account.storage().to_commitment();
    let account_commitment_1 = account.commitment();

    // First update with original account - full state delta
    let delta_1 = AccountDelta::try_from(account).unwrap();

    let account_update_1 = BlockAccountUpdate::new(
        account_id,
        account_commitment_1,
        AccountUpdateDetails::Delta(delta_1),
    );

    upsert_accounts(&mut conn, &[account_update_1], block_num_1).expect("First upsert failed");

    // Create modified account with different storage value
    let storage_value_modified =
        Word::from([Felt::new(10), Felt::new(20), Felt::new(30), Felt::new(40)]);
    let component_storage_modified =
        vec![StorageSlot::with_value(StorageSlotName::mock(0), storage_value_modified)];

    let account_component_code = CodeBuilder::default()
        .compile_component_code("test::interface", "pub proc foo push.1 end")
        .unwrap();

    let component_2 = AccountComponent::new(account_component_code, component_storage_modified)
        .unwrap()
        .with_supported_type(AccountType::RegularAccountImmutableCode);

    let account_2 = AccountBuilder::new([1u8; 32])
        .account_type(AccountType::RegularAccountImmutableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_component(component_2)
        .with_auth_component(AuthRpoFalcon512::new(PublicKeyCommitment::from(EMPTY_WORD)))
        .build_existing()
        .unwrap();

    let storage_commitment_2 = account_2.storage().to_commitment();
    let account_commitment_2 = account_2.commitment();

    // Second update with modified account - full state delta
    let delta_2 = AccountDelta::try_from(account_2).unwrap();

    let account_update_2 = BlockAccountUpdate::new(
        account_id,
        account_commitment_2,
        AccountUpdateDetails::Delta(delta_2),
    );

    upsert_accounts(&mut conn, &[account_update_2], block_num_2).expect("Second upsert failed");

    // Verify 2 total account rows exist (both historical records)
    let total_accounts: i64 = schema::accounts::table
        .filter(schema::accounts::account_id.eq(account_id.to_bytes()))
        .count()
        .get_result(&mut conn)
        .expect("Failed to count total accounts");

    assert_eq!(total_accounts, 2, "Expected 2 total account records");

    // Verify only 1 is marked as latest
    let latest_accounts: i64 = schema::accounts::table
        .filter(schema::accounts::account_id.eq(account_id.to_bytes()))
        .filter(schema::accounts::is_latest.eq(true))
        .count()
        .get_result(&mut conn)
        .expect("Failed to count latest accounts");

    assert_eq!(latest_accounts, 1, "Expected exactly 1 latest account");

    // Verify latest storage matches second update
    let latest_storage = select_latest_account_storage(&mut conn, account_id)
        .expect("Failed to query latest storage");

    assert_eq!(
        latest_storage.to_commitment(),
        storage_commitment_2,
        "Latest storage should match second update"
    );

    // Verify historical query returns first update
    let storage_at_block_1 = select_account_storage_at_block(&mut conn, account_id, block_num_1)
        .expect("Failed to query storage at block 1");

    assert_eq!(
        storage_at_block_1.to_commitment(),
        storage_commitment_1,
        "Storage at block 1 should match first update"
    );
}

#[test]
fn test_upsert_accounts_with_multiple_storage_slots() {
    let mut conn = setup_test_db();

    // Create account with 3 storage slots
    let account_id = AccountId::dummy(
        [2u8; 15],
        AccountIdVersion::Version0,
        AccountType::RegularAccountImmutableCode,
        AccountStorageMode::Public,
    );

    let slot_value_1 = Word::from([Felt::new(1), Felt::new(2), Felt::new(3), Felt::new(4)]);
    let slot_value_2 = Word::from([Felt::new(5), Felt::new(6), Felt::new(7), Felt::new(8)]);
    let slot_value_3 = Word::from([Felt::new(9), Felt::new(10), Felt::new(11), Felt::new(12)]);

    let component_storage = vec![
        StorageSlot::with_value(StorageSlotName::mock(0), slot_value_1),
        StorageSlot::with_value(StorageSlotName::mock(1), slot_value_2),
        StorageSlot::with_value(StorageSlotName::mock(2), slot_value_3),
    ];

    let account_component_code = CodeBuilder::default()
        .compile_component_code("test::interface", "pub proc foo push.1 end")
        .unwrap();

    let component = AccountComponent::new(account_component_code, component_storage)
        .unwrap()
        .with_supported_type(AccountType::RegularAccountImmutableCode);

    let account = AccountBuilder::new([2u8; 32])
        .account_type(AccountType::RegularAccountImmutableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_component(component)
        .with_auth_component(AuthRpoFalcon512::new(PublicKeyCommitment::from(EMPTY_WORD)))
        .build_existing()
        .unwrap();

    let block_num = BlockNumber::from_epoch(0);
    insert_block_header(&mut conn, block_num);

    let storage_commitment = account.storage().to_commitment();
    let account_commitment = account.commitment();
    let delta = AccountDelta::try_from(account).unwrap();

    let account_update =
        BlockAccountUpdate::new(account_id, account_commitment, AccountUpdateDetails::Delta(delta));

    upsert_accounts(&mut conn, &[account_update], block_num)
        .expect("Upsert with multiple storage slots failed");

    // Query back and verify
    let queried_storage =
        select_latest_account_storage(&mut conn, account_id).expect("Failed to query storage");

    assert_eq!(
        queried_storage.to_commitment(),
        storage_commitment,
        "Storage commitment mismatch"
    );

    // Note: Auth component adds 1 storage slot, so 3 component slots + 1 auth = 4 total
    assert_eq!(
        queried_storage.slots().len(),
        4,
        "Expected 4 storage slots (3 component + 1 auth)"
    );

    // The storage commitment matching proves that all values are correctly preserved.
    // We don't check individual slot values by index since slot ordering may vary.
}

#[test]
fn test_upsert_accounts_with_empty_storage() {
    let mut conn = setup_test_db();

    // Create account with no component storage slots (only auth slot)
    let account_id = AccountId::dummy(
        [3u8; 15],
        AccountIdVersion::Version0,
        AccountType::RegularAccountImmutableCode,
        AccountStorageMode::Public,
    );

    let account_component_code = CodeBuilder::default()
        .compile_component_code("test::interface", "pub proc foo push.1 end")
        .unwrap();

    let component = AccountComponent::new(account_component_code, vec![])
        .unwrap()
        .with_supported_type(AccountType::RegularAccountImmutableCode);

    let account = AccountBuilder::new([3u8; 32])
        .account_type(AccountType::RegularAccountImmutableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_component(component)
        .with_auth_component(AuthRpoFalcon512::new(PublicKeyCommitment::from(EMPTY_WORD)))
        .build_existing()
        .unwrap();

    let block_num = BlockNumber::from_epoch(0);
    insert_block_header(&mut conn, block_num);

    let storage_commitment = account.storage().to_commitment();
    let account_commitment = account.commitment();
    let delta = AccountDelta::try_from(account).unwrap();

    let account_update =
        BlockAccountUpdate::new(account_id, account_commitment, AccountUpdateDetails::Delta(delta));

    upsert_accounts(&mut conn, &[account_update], block_num)
        .expect("Upsert with empty storage failed");

    // Query back and verify
    let queried_storage =
        select_latest_account_storage(&mut conn, account_id).expect("Failed to query storage");

    assert_eq!(
        queried_storage.to_commitment(),
        storage_commitment,
        "Storage commitment mismatch for empty storage"
    );

    // Note: Auth component adds 1 storage slot, so even "empty" accounts have 1 slot
    assert_eq!(queried_storage.slots().len(), 1, "Expected 1 storage slot (auth component)");

    // Verify the storage header blob exists in database
    let storage_header_exists: Option<bool> = SelectDsl::select(
        schema::accounts::table
            .filter(schema::accounts::account_id.eq(account_id.to_bytes()))
            .filter(schema::accounts::is_latest.eq(true)),
        schema::accounts::storage_header.is_not_null(),
    )
    .first(&mut conn)
    .optional()
    .expect("Failed to check storage header existence");

    assert_eq!(
        storage_header_exists,
        Some(true),
        "Storage header blob should exist even for empty storage"
    );
}

// VAULT AT BLOCK HISTORICAL QUERY TESTS
// ================================================================================================

/// Tests that querying vault at an older block returns the correct historical state,
/// even when the same vault_key has been updated in later blocks.
///
/// This tests the deduplication logic that relies on ordering by (vault_key ASC, block_num DESC).
#[test]
fn test_select_account_vault_at_block_historical_with_updates() {
    use assert_matches::assert_matches;
    use miden_protocol::asset::{AssetVaultKey, FungibleAsset};
    use miden_protocol::testing::account_id::ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET;

    let mut conn = setup_test_db();
    let (account, _) = create_test_account_with_storage();
    let account_id = account.id();

    // Faucet ID is needed for creating FungibleAssets
    let faucet_id = AccountId::try_from(ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET).unwrap();

    let block_1 = BlockNumber::from_epoch(0);
    let block_2 = BlockNumber::from_epoch(1);
    let block_3 = BlockNumber::from_epoch(2);

    insert_block_header(&mut conn, block_1);
    insert_block_header(&mut conn, block_2);
    insert_block_header(&mut conn, block_3);

    // Insert account at block 1
    let delta = AccountDelta::try_from(account.clone()).unwrap();
    let account_update = BlockAccountUpdate::new(
        account_id,
        account.commitment(),
        AccountUpdateDetails::Delta(delta),
    );
    upsert_accounts(&mut conn, &[account_update], block_1).expect("upsert_accounts failed");

    // Insert vault asset at block 1: vault_key_1 = 1000 tokens
    let vault_key_1 = AssetVaultKey::new_unchecked(Word::from([
        Felt::new(1),
        Felt::new(0),
        Felt::new(0),
        Felt::new(0),
    ]));
    let asset_v1 = Asset::Fungible(FungibleAsset::new(faucet_id, 1000).unwrap());

    insert_account_vault_asset(&mut conn, account_id, block_1, vault_key_1, Some(asset_v1))
        .expect("insert vault asset failed");

    // Update vault asset at block 2: vault_key_1 = 2000 tokens (updated value)
    let asset_v2 = Asset::Fungible(FungibleAsset::new(faucet_id, 2000).unwrap());
    insert_account_vault_asset(&mut conn, account_id, block_2, vault_key_1, Some(asset_v2))
        .expect("insert vault asset update failed");

    // Add a second vault_key at block 2
    let vault_key_2 = AssetVaultKey::new_unchecked(Word::from([
        Felt::new(2),
        Felt::new(0),
        Felt::new(0),
        Felt::new(0),
    ]));
    let asset_key2 = Asset::Fungible(FungibleAsset::new(faucet_id, 500).unwrap());
    insert_account_vault_asset(&mut conn, account_id, block_2, vault_key_2, Some(asset_key2))
        .expect("insert second vault asset failed");

    // Update vault_key_1 again at block 3: vault_key_1 = 3000 tokens
    let asset_v3 = Asset::Fungible(FungibleAsset::new(faucet_id, 3000).unwrap());
    insert_account_vault_asset(&mut conn, account_id, block_3, vault_key_1, Some(asset_v3))
        .expect("insert vault asset update 2 failed");

    // Query at block 1: should only see vault_key_1 with 1000 tokens
    let assets_at_block_1 = select_account_vault_at_block(&mut conn, account_id, block_1)
        .expect("Query at block 1 should succeed");

    assert_eq!(assets_at_block_1.len(), 1, "Should have 1 asset at block 1");
    assert_matches!(&assets_at_block_1[0], Asset::Fungible(f) if f.amount() == 1000);

    // Query at block 2: should see vault_key_1 with 2000 tokens AND vault_key_2 with 500 tokens
    let assets_at_block_2 = select_account_vault_at_block(&mut conn, account_id, block_2)
        .expect("Query at block 2 should succeed");

    assert_eq!(assets_at_block_2.len(), 2, "Should have 2 assets at block 2");

    // Find the amounts (order may vary)
    let amounts: Vec<u64> = assets_at_block_2
        .iter()
        .map(|a| {
            assert_matches!(a, Asset::Fungible(f) => f.amount())
        })
        .collect();

    assert!(amounts.contains(&2000), "Block 2 should have vault_key_1 with 2000 tokens");
    assert!(amounts.contains(&500), "Block 2 should have vault_key_2 with 500 tokens");

    // Query at block 3: should see vault_key_1 with 3000 tokens AND vault_key_2 with 500 tokens
    let assets_at_block_3 = select_account_vault_at_block(&mut conn, account_id, block_3)
        .expect("Query at block 3 should succeed");

    assert_eq!(assets_at_block_3.len(), 2, "Should have 2 assets at block 3");

    let amounts: Vec<u64> = assets_at_block_3
        .iter()
        .map(|a| {
            assert_matches!(a, Asset::Fungible(f) => f.amount())
        })
        .collect();

    assert!(amounts.contains(&3000), "Block 3 should have vault_key_1 with 3000 tokens");
    assert!(amounts.contains(&500), "Block 3 should have vault_key_2 with 500 tokens");
}

/// Tests that deleted vault assets (asset = None) are correctly excluded from results,
/// and that the deduplication handles deletion entries properly.
#[test]
fn test_select_account_vault_at_block_with_deletion() {
    use assert_matches::assert_matches;
    use miden_protocol::asset::{AssetVaultKey, FungibleAsset};
    use miden_protocol::testing::account_id::ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET;

    let mut conn = setup_test_db();
    let (account, _) = create_test_account_with_storage();
    let account_id = account.id();

    // Faucet ID is needed for creating FungibleAssets
    let faucet_id = AccountId::try_from(ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET).unwrap();

    let block_1 = BlockNumber::from_epoch(0);
    let block_2 = BlockNumber::from_epoch(1);
    let block_3 = BlockNumber::from_epoch(2);

    insert_block_header(&mut conn, block_1);
    insert_block_header(&mut conn, block_2);
    insert_block_header(&mut conn, block_3);

    // Insert account at block 1
    let delta = AccountDelta::try_from(account.clone()).unwrap();
    let account_update = BlockAccountUpdate::new(
        account_id,
        account.commitment(),
        AccountUpdateDetails::Delta(delta),
    );
    upsert_accounts(&mut conn, &[account_update], block_1).expect("upsert_accounts failed");

    // Insert vault asset at block 1
    let vault_key = AssetVaultKey::new_unchecked(Word::from([
        Felt::new(1),
        Felt::new(0),
        Felt::new(0),
        Felt::new(0),
    ]));
    let asset = Asset::Fungible(FungibleAsset::new(faucet_id, 1000).unwrap());

    insert_account_vault_asset(&mut conn, account_id, block_1, vault_key, Some(asset))
        .expect("insert vault asset failed");

    // Delete the vault asset at block 2 (insert with asset = None)
    insert_account_vault_asset(&mut conn, account_id, block_2, vault_key, None)
        .expect("delete vault asset failed");

    // Re-add the vault asset at block 3 with different amount
    let asset_v3 = Asset::Fungible(FungibleAsset::new(faucet_id, 2000).unwrap());
    insert_account_vault_asset(&mut conn, account_id, block_3, vault_key, Some(asset_v3))
        .expect("re-add vault asset failed");

    // Query at block 1: should see the asset
    let assets_at_block_1 = select_account_vault_at_block(&mut conn, account_id, block_1)
        .expect("Query at block 1 should succeed");
    assert_eq!(assets_at_block_1.len(), 1, "Should have 1 asset at block 1");

    // Query at block 2: should NOT see the asset (it was deleted)
    let assets_at_block_2 = select_account_vault_at_block(&mut conn, account_id, block_2)
        .expect("Query at block 2 should succeed");
    assert!(assets_at_block_2.is_empty(), "Should have no assets at block 2 (deleted)");

    // Query at block 3: should see the re-added asset with new amount
    let assets_at_block_3 = select_account_vault_at_block(&mut conn, account_id, block_3)
        .expect("Query at block 3 should succeed");
    assert_eq!(assets_at_block_3.len(), 1, "Should have 1 asset at block 3");
    assert_matches!(&assets_at_block_3[0], Asset::Fungible(f) if f.amount() == 2000);
}
