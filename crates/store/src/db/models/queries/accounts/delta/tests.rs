//! Tests for delta update functionality.

use std::collections::BTreeMap;

use assert_matches::assert_matches;
use diesel::{Connection, ExpressionMethods, QueryDsl, RunQueryDsl, SqliteConnection};
use diesel_migrations::MigrationHarness;
use miden_node_utils::fee::test_fee_params;
use miden_protocol::account::auth::PublicKeyCommitment;
use miden_protocol::account::delta::{
    AccountStorageDelta,
    AccountUpdateDetails,
    AccountVaultDelta,
    StorageSlotDelta,
};
use miden_protocol::account::{
    Account,
    AccountBuilder,
    AccountComponent,
    AccountDelta,
    AccountId,
    AccountStorage,
    AccountStorageMode,
    AccountType,
    StorageSlot,
    StorageSlotName,
};
use miden_protocol::asset::{Asset, FungibleAsset};
use miden_protocol::block::{BlockAccountUpdate, BlockHeader, BlockNumber};
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SecretKey;
use miden_protocol::testing::account_id::ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET;
use miden_protocol::utils::Serializable;
use miden_protocol::{EMPTY_WORD, Felt, Word};
use miden_standards::account::auth::AuthFalcon512Rpo;
use miden_standards::code_builder::CodeBuilder;

use crate::db::migrations::MIGRATIONS;
use crate::db::models::queries::accounts::{
    select_account_header_with_storage_header_at_block,
    select_account_vault_at_block,
    select_full_account,
    upsert_accounts,
};
use crate::db::schema::accounts;

fn setup_test_db() -> SqliteConnection {
    let mut conn =
        SqliteConnection::establish(":memory:").expect("Failed to create in-memory database");

    conn.run_pending_migrations(MIGRATIONS).expect("Failed to run migrations");

    conn
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

/// Tests that the optimized delta update path produces the same results as the old
/// method that loads the full account.
///
/// Covers partial deltas that update:
/// - Nonce (via `nonce_delta`)
/// - Value storage slots
/// - Vault assets (fungible) starting from empty vault
///
/// The test ensures the optimized code path in `upsert_accounts` produces correct results
/// by comparing the final account state against a manually constructed expected state.
#[test]
#[allow(clippy::too_many_lines)]
fn optimized_delta_matches_full_account_method() {
    let mut conn = setup_test_db();

    // Create an account with value slots only (no map slots to avoid SmtForest complexity)
    let slot_value_initial =
        Word::from([Felt::new(100), Felt::new(200), Felt::new(300), Felt::new(400)]);

    let component_storage = vec![
        StorageSlot::with_value(StorageSlotName::mock(0), slot_value_initial),
        StorageSlot::with_value(StorageSlotName::mock(1), EMPTY_WORD),
    ];

    let account_component_code = CodeBuilder::default()
        .compile_component_code("test::interface", "pub proc foo push.1 end")
        .unwrap();

    let component = AccountComponent::new(account_component_code, component_storage)
        .unwrap()
        .with_supported_type(AccountType::RegularAccountImmutableCode);

    let account = AccountBuilder::new([10u8; 32])
        .account_type(AccountType::RegularAccountImmutableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_component(component)
        .with_auth_component(AuthFalcon512Rpo::new(PublicKeyCommitment::from(EMPTY_WORD)))
        .build_existing()
        .unwrap();

    let block_1 = BlockNumber::from(1);
    let block_2 = BlockNumber::from(2);
    insert_block_header(&mut conn, block_1);
    insert_block_header(&mut conn, block_2);

    // Insert the initial account at block 1 (full state) - no vault assets
    let delta_initial = AccountDelta::try_from(account.clone()).unwrap();
    let account_update_initial = BlockAccountUpdate::new(
        account.id(),
        account.commitment(),
        AccountUpdateDetails::Delta(delta_initial),
    );
    upsert_accounts(&mut conn, &[account_update_initial], block_1).expect("Initial upsert failed");

    // Verify initial state
    let full_account_before =
        select_full_account(&mut conn, account.id()).expect("Failed to load full account");
    assert_eq!(full_account_before.nonce(), account.nonce());
    assert!(
        full_account_before.vault().assets().next().is_none(),
        "Vault should be empty initially"
    );

    // Create a partial delta to apply:
    // - Increment nonce by 5
    // - Update the first value slot
    // - Add 500 tokens to the vault (starting from empty)

    let new_slot_value =
        Word::from([Felt::new(111), Felt::new(222), Felt::new(333), Felt::new(444)]);
    let faucet_id = AccountId::try_from(ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET).unwrap();

    // Find the slot name from the account's storage
    let value_slot_name =
        full_account_before.storage().slots().iter().next().unwrap().name().clone();

    // Build the storage delta (value slot update only)
    let storage_delta = {
        let deltas = BTreeMap::from_iter([(
            value_slot_name.clone(),
            StorageSlotDelta::Value(new_slot_value),
        )]);
        AccountStorageDelta::from_raw(deltas)
    };

    // Build the vault delta (add 500 tokens to empty vault)
    let vault_delta = {
        let mut delta = AccountVaultDelta::default();
        let asset = Asset::Fungible(FungibleAsset::new(faucet_id, 500).unwrap());
        delta.add_asset(asset).unwrap();
        delta
    };

    // Create a partial delta
    let nonce_delta = Felt::new(5);
    let partial_delta = AccountDelta::new(
        full_account_before.id(),
        storage_delta.clone(),
        vault_delta.clone(),
        nonce_delta,
    )
    .unwrap();
    assert!(!partial_delta.is_full_state(), "Delta should be partial, not full state");

    // Construct the expected final account by applying the delta
    let expected_nonce = Felt::new(full_account_before.nonce().as_int() + nonce_delta.as_int());
    let expected_code_commitment = full_account_before.code().commitment();

    let mut expected_account = full_account_before.clone();
    expected_account.apply_delta(&partial_delta).unwrap();
    let final_account_for_commitment = expected_account;

    let final_commitment = final_account_for_commitment.commitment();
    let expected_storage_commitment = final_account_for_commitment.storage().to_commitment();
    let expected_vault_root = final_account_for_commitment.vault().root();

    // ----- Apply the partial delta via upsert_accounts (optimized path) -----
    let account_update = BlockAccountUpdate::new(
        account.id(),
        final_commitment,
        AccountUpdateDetails::Delta(partial_delta),
    );
    upsert_accounts(&mut conn, &[account_update], block_2).expect("Partial delta upsert failed");

    // ----- VERIFY: Query the DB and check that optimized path produced correct results -----

    let (header_after, storage_header_after) =
        select_account_header_with_storage_header_at_block(&mut conn, account.id(), block_2)
            .expect("Query should succeed")
            .expect("Account should exist");

    // Verify nonce
    assert_eq!(
        header_after.nonce(),
        expected_nonce,
        "Nonce mismatch: optimized={:?}, expected={:?}",
        header_after.nonce(),
        expected_nonce
    );

    // Verify code commitment (should be unchanged)
    assert_eq!(
        header_after.code_commitment(),
        expected_code_commitment,
        "Code commitment mismatch"
    );

    // Verify storage header commitment
    assert_eq!(
        storage_header_after.to_commitment(),
        expected_storage_commitment,
        "Storage header commitment mismatch"
    );

    // Verify vault assets
    let vault_assets_after = select_account_vault_at_block(&mut conn, account.id(), block_2)
        .expect("Query vault should succeed");

    assert_eq!(vault_assets_after.len(), 1, "Should have 1 vault asset");
    assert_matches!(&vault_assets_after[0], Asset::Fungible(f) => {
        assert_eq!(f.faucet_id(), faucet_id, "Faucet ID should match");
        assert_eq!(f.amount(), 500, "Amount should be 500");
    });

    // Verify the account commitment matches
    assert_eq!(
        header_after.commitment(),
        final_commitment,
        "Account commitment should match the expected final state"
    );

    // Also verify we can load the full account and it has correct state
    let full_account_after = select_full_account(&mut conn, account.id())
        .expect("Failed to load full account after update");

    assert_eq!(full_account_after.nonce(), expected_nonce, "Full account nonce mismatch");
    assert_eq!(
        full_account_after.storage().to_commitment(),
        expected_storage_commitment,
        "Full account storage commitment mismatch"
    );
    assert_eq!(
        full_account_after.vault().root(),
        expected_vault_root,
        "Full account vault root mismatch"
    );
}

/// Tests that a private account update (no public state) is handled correctly.
///
/// Private accounts store only the account commitment, not the full state.
#[test]
fn upsert_private_account() {
    use miden_protocol::account::{AccountIdVersion, AccountStorageMode, AccountType};

    let mut conn = setup_test_db();

    let block_num = BlockNumber::from(1);
    insert_block_header(&mut conn, block_num);

    // Create a private account ID
    let account_id = AccountId::dummy(
        [20u8; 15],
        AccountIdVersion::Version0,
        AccountType::RegularAccountImmutableCode,
        AccountStorageMode::Private,
    );

    let account_commitment = Word::from([Felt::new(1), Felt::new(2), Felt::new(3), Felt::new(4)]);

    // Insert as private account
    let account_update =
        BlockAccountUpdate::new(account_id, account_commitment, AccountUpdateDetails::Private);

    upsert_accounts(&mut conn, &[account_update], block_num)
        .expect("Private account upsert failed");

    // Verify the account exists and commitment matches

    let (stored_commitment, stored_nonce, stored_code): (Vec<u8>, Option<i64>, Option<Vec<u8>>) =
        accounts::table
            .filter(accounts::account_id.eq(account_id.to_bytes()))
            .filter(accounts::is_latest.eq(true))
            .select((accounts::account_commitment, accounts::nonce, accounts::code_commitment))
            .first(&mut conn)
            .expect("Account should exist in DB");

    assert_eq!(
        stored_commitment,
        account_commitment.to_bytes(),
        "Stored commitment should match"
    );

    // Private accounts have NULL for nonce, code_commitment, storage_header, vault_root
    assert!(stored_nonce.is_none(), "Private account should have NULL nonce");
    assert!(stored_code.is_none(), "Private account should have NULL code_commitment");
}

/// Tests that a full-state delta (new account creation) is handled correctly.
///
/// Full-state deltas contain the complete account state including code.
#[test]
fn upsert_full_state_delta() {
    let mut conn = setup_test_db();

    let block_num = BlockNumber::from(1);
    insert_block_header(&mut conn, block_num);

    // Create an account with storage
    let slot_value = Word::from([Felt::new(10), Felt::new(20), Felt::new(30), Felt::new(40)]);
    let component_storage = vec![StorageSlot::with_value(StorageSlotName::mock(0), slot_value)];

    let account_component_code = CodeBuilder::default()
        .compile_component_code("test::interface", "pub proc bar push.2 end")
        .unwrap();

    let component = AccountComponent::new(account_component_code, component_storage)
        .unwrap()
        .with_supported_type(AccountType::RegularAccountImmutableCode);

    let account = AccountBuilder::new([20u8; 32])
        .account_type(AccountType::RegularAccountImmutableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_component(component)
        .with_auth_component(AuthFalcon512Rpo::new(PublicKeyCommitment::from(EMPTY_WORD)))
        .build_existing()
        .unwrap();

    // Create a full-state delta from the account
    let delta = AccountDelta::try_from(account.clone()).unwrap();
    assert!(delta.is_full_state(), "Delta should be full state");

    let account_update = BlockAccountUpdate::new(
        account.id(),
        account.commitment(),
        AccountUpdateDetails::Delta(delta),
    );

    upsert_accounts(&mut conn, &[account_update], block_num)
        .expect("Full-state delta upsert failed");

    // Verify the account state was stored correctly
    let (header, storage_header) =
        select_account_header_with_storage_header_at_block(&mut conn, account.id(), block_num)
            .expect("Query should succeed")
            .expect("Account should exist");

    assert_eq!(header.nonce(), account.nonce(), "Nonce should match");
    assert_eq!(
        header.code_commitment(),
        account.code().commitment(),
        "Code commitment should match"
    );
    assert_eq!(
        storage_header.to_commitment(),
        account.storage().to_commitment(),
        "Storage commitment should match"
    );

    // Verify we can load the full account back
    let loaded_account =
        select_full_account(&mut conn, account.id()).expect("Should load full account");

    assert_eq!(loaded_account.nonce(), account.nonce());
    assert_eq!(loaded_account.code().commitment(), account.code().commitment());
    assert_eq!(loaded_account.storage().to_commitment(), account.storage().to_commitment());
}
