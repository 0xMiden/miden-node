use assert_matches::assert_matches;
use diesel::{Connection, RunQueryDsl};
use diesel_migrations::MigrationHarness;
use miden_lib::account::auth::AuthRpoFalcon512;
use miden_lib::transaction::TransactionKernel;
use miden_node_utils::fee::test_fee_params;
use miden_objects::account::auth::PublicKeyCommitment;
use miden_objects::account::{
    AccountBuilder,
    AccountComponent,
    AccountIdVersion,
    AccountStorageMode,
    AccountType,
    StorageSlot,
};
use miden_objects::{EMPTY_WORD, Word};

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
    let component_storage = vec![StorageSlot::Value(storage_value)];

    let component = AccountComponent::compile(
        "export.foo push.1 end",
        TransactionKernel::assembler(),
        component_storage,
    )
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
    use miden_objects::block::BlockHeader;

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
        Word::default(),
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

#[test]
fn test_upsert_accounts_inserts_storage_header() {
    let mut conn = setup_test_db();
    let (account, account_id) = create_test_account_with_storage();

    // Block 1
    let block_num = BlockNumber::from_epoch(0);
    insert_block_header(&mut conn, block_num);

    let storage_commitment_original = account.storage().commitment();
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
        queried_storage.commitment(),
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
    let storage_commitment_1 = account.storage().commitment();
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
    let component_storage_modified = vec![StorageSlot::Value(storage_value_modified)];

    let component_2 = AccountComponent::compile(
        "export.foo push.1 end",
        TransactionKernel::assembler(),
        component_storage_modified,
    )
    .unwrap()
    .with_supported_type(AccountType::RegularAccountImmutableCode);

    let account_2 = AccountBuilder::new([1u8; 32])
        .account_type(AccountType::RegularAccountImmutableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_component(component_2)
        .with_auth_component(AuthRpoFalcon512::new(PublicKeyCommitment::from(EMPTY_WORD)))
        .build_existing()
        .unwrap();

    let storage_commitment_2 = account_2.storage().commitment();
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
        latest_storage.commitment(),
        storage_commitment_2,
        "Latest storage should match second update"
    );

    // Verify historical query returns first update
    let storage_at_block_1 = select_account_storage_at_block(&mut conn, account_id, block_num_1)
        .expect("Failed to query storage at block 1");

    assert_eq!(
        storage_at_block_1.commitment(),
        storage_commitment_1,
        "Storage at block 1 should match first update"
    );
}

#[test]
fn test_upsert_accounts_with_incremental_delta() {
    use std::collections::BTreeMap;

    use miden_objects::account::delta::{AccountStorageDelta, AccountVaultDelta};

    let mut conn = setup_test_db();
    let (account, account_id) = create_test_account_with_storage();

    let block_num_1 = BlockNumber::from_epoch(0);
    let block_num_2 = BlockNumber::from_epoch(1);

    insert_block_header(&mut conn, block_num_1);
    insert_block_header(&mut conn, block_num_2);

    // First update with full state
    let storage_commitment_1 = account.storage().commitment();
    let account_commitment_1 = account.commitment();
    let nonce_1 = account.nonce();
    let delta_1 = AccountDelta::try_from(account).unwrap();

    let account_update_1 = BlockAccountUpdate::new(
        account_id,
        account_commitment_1,
        AccountUpdateDetails::Delta(delta_1),
    );

    upsert_accounts(&mut conn, &[account_update_1], block_num_1).expect("First upsert failed");

    // Create incremental delta (only modify storage value slot 1)
    let new_storage_value =
        Word::from([Felt::new(100), Felt::new(200), Felt::new(300), Felt::new(400)]);

    let mut storage_delta_values = BTreeMap::new();
    storage_delta_values.insert(1u8, new_storage_value); // Update slot 1 (component storage)

    let storage_delta = AccountStorageDelta::from_parts(storage_delta_values, BTreeMap::new())
        .expect("Failed to create storage delta");
    let incremental_delta =
        AccountDelta::new(account_id, storage_delta, AccountVaultDelta::default(), nonce_1)
            .expect("Failed to create incremental delta");

    // Reconstruct expected account after delta
    let account_after = reconstruct_full_account_from_db(&mut conn, account_id)
        .expect("Failed to reconstruct account");
    let mut expected_account = account_after.clone();
    expected_account
        .apply_delta(&incremental_delta)
        .expect("Failed to apply delta to expected account");

    let storage_commitment_2 = expected_account.storage().commitment();
    let account_commitment_2 = expected_account.commitment();

    let account_update_2 = BlockAccountUpdate::new(
        account_id,
        account_commitment_2,
        AccountUpdateDetails::Delta(incremental_delta),
    );

    upsert_accounts(&mut conn, &[account_update_2], block_num_2)
        .expect("Second upsert with incremental delta failed");

    // Verify latest storage matches expected state
    let latest_storage = select_latest_account_storage(&mut conn, account_id)
        .expect("Failed to query latest storage");

    assert_eq!(
        latest_storage.commitment(),
        storage_commitment_2,
        "Storage commitment should match after incremental delta"
    );

    // Verify historical storage is preserved
    let storage_at_block_1 = select_account_storage_at_block(&mut conn, account_id, block_num_1)
        .expect("Failed to query storage at block 1");

    assert_eq!(
        storage_at_block_1.commitment(),
        storage_commitment_1,
        "Historical storage should be unchanged"
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
        StorageSlot::Value(slot_value_1),
        StorageSlot::Value(slot_value_2),
        StorageSlot::Value(slot_value_3),
    ];

    let component = AccountComponent::compile(
        "export.foo push.1 end",
        TransactionKernel::assembler(),
        component_storage,
    )
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

    let storage_commitment = account.storage().commitment();
    let account_commitment = account.commitment();
    let delta = AccountDelta::try_from(account).unwrap();

    let account_update =
        BlockAccountUpdate::new(account_id, account_commitment, AccountUpdateDetails::Delta(delta));

    upsert_accounts(&mut conn, &[account_update], block_num)
        .expect("Upsert with multiple storage slots failed");

    // Query back and verify
    let queried_storage =
        select_latest_account_storage(&mut conn, account_id).expect("Failed to query storage");

    assert_eq!(queried_storage.commitment(), storage_commitment, "Storage commitment mismatch");

    // Note: Auth component adds 1 storage slot, so 3 component slots + 1 auth = 4 total
    assert_eq!(
        queried_storage.slots().len(),
        4,
        "Expected 4 storage slots (3 component + 1 auth)"
    );

    // Verify individual slot values (skipping auth slot at index 0)
    assert_matches!(
        queried_storage.slots().get(1).expect("Slot 1 should exist"),
        &StorageSlot::Value(v) if v == slot_value_1,
        "Slot 1 value mismatch"
    );
    assert_matches!(
        queried_storage.slots().get(2).expect("Slot 2 should exist"),
        &StorageSlot::Value(v) if v == slot_value_2,
        "Slot 2 value mismatch"
    );
    assert_matches!(
        queried_storage.slots().get(3).expect("Slot 3 should exist"),
        &StorageSlot::Value(v) if v == slot_value_3,
        "Slot 3 value mismatch"
    );
}

#[test]
fn test_upsert_accounts_with_empty_storage() {
    let mut conn = setup_test_db();

    // Create account with no storage slots
    let account_id = AccountId::dummy(
        [3u8; 15],
        AccountIdVersion::Version0,
        AccountType::RegularAccountImmutableCode,
        AccountStorageMode::Public,
    );

    let component = AccountComponent::compile(
        "export.foo push.1 end",
        TransactionKernel::assembler(),
        vec![], // Empty storage
    )
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

    let storage_commitment = account.storage().commitment();
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
        queried_storage.commitment(),
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
