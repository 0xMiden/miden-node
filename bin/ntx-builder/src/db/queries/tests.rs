//! DB-level tests for the committed-block-driven query layer.
//!
//! Each query runs through the framework's [`Database::read`]/[`Database::write`], so every write
//! commits before the following read observes it.

use std::sync::Arc;

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::Database;
use miden_protocol::Word;
use miden_protocol::account::{Account, AccountId};
use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::crypto::merkle::mmr::PartialMmr;
use miden_protocol::note::{NoteId, NoteScript, Nullifier};
use miden_protocol::transaction::TransactionId;
use miden_standards::note::AccountTargetNetworkNote;

use super::*;
use crate::NoteError;
use crate::committed_block::CommittedBlockEffects;
use crate::db::test_setup;
use crate::test_utils::*;

// TEST HARNESS
// ================================================================================================

/// Creates a [`NoteError`] from a string message, for use in tests.
fn test_note_error(msg: &str) -> NoteError {
    Arc::new(std::io::Error::other(msg.to_string()))
}

/// Counts the rows returned by a `SELECT COUNT(*)` statement.
async fn count(db: &Database, sql: &'static str) -> i64 {
    db.read("count", move |tx| {
        let n = tx.query(sql, &[], |row| row.get::<i64>(0))?.into_iter().next().unwrap_or(0);
        Ok::<i64, DatabaseError>(n)
    })
    .await
    .unwrap()
}

async fn count_notes(db: &Database) -> i64 {
    count(db, "SELECT COUNT(*) FROM notes").await
}

async fn count_accounts(db: &Database) -> i64 {
    count(db, "SELECT COUNT(*) FROM accounts").await
}

async fn count_chain_state(db: &Database) -> i64 {
    count(db, "SELECT COUNT(*) FROM chain_state").await
}

async fn do_upsert_account(
    db: &Database,
    account_id: AccountId,
    account: Account,
    last_tx_id: TransactionId,
) {
    db.write("upsert_account", move |tx| upsert_account(tx, account_id, &account, last_tx_id))
        .await
        .unwrap();
}

async fn do_get_account(db: &Database, account_id: AccountId) -> Option<Account> {
    db.read("get_account", move |tx| get_account(tx, account_id)).await.unwrap()
}

async fn do_account_last_tx(db: &Database, account_id: AccountId) -> Option<TransactionId> {
    db.read("account_last_tx", move |tx| account_last_tx(tx, account_id))
        .await
        .unwrap()
}

async fn do_insert_notes(db: &Database, notes: Vec<AccountTargetNetworkNote>) {
    db.write("insert_network_notes", move |tx| insert_network_notes(tx, &notes))
        .await
        .unwrap();
}

async fn do_mark_consumed(db: &Database, nullifiers: Vec<Nullifier>, block_num: BlockNumber) {
    db.write("mark_notes_consumed", move |tx| mark_notes_consumed(tx, &nullifiers, block_num))
        .await
        .unwrap();
}

async fn do_available_notes(
    db: &Database,
    account_id: AccountId,
    block_num: BlockNumber,
    max_attempts: usize,
) -> Vec<AccountTargetNetworkNote> {
    db.read("available_notes", move |tx| {
        available_notes(tx, account_id, block_num, max_attempts)
    })
    .await
    .unwrap()
}

async fn do_notes_failed(
    db: &Database,
    failed: Vec<(Nullifier, NoteError)>,
    block_num: BlockNumber,
) {
    db.write("notes_failed", move |tx| notes_failed(tx, &failed, block_num))
        .await
        .unwrap();
}

async fn do_discard_notes(
    db: &Database,
    nullifiers: Vec<Nullifier>,
    block_num: BlockNumber,
    max_attempts: usize,
    reason: &str,
) {
    let reason = reason.to_string();
    db.write("discard_notes", move |tx| {
        discard_notes(tx, &nullifiers, block_num, max_attempts, &reason)
    })
    .await
    .unwrap();
}

async fn do_get_note_status(db: &Database, note_id: NoteId) -> Option<NoteStatusRow> {
    db.read("get_note_status", move |tx| get_note_status(tx, note_id))
        .await
        .unwrap()
}

async fn do_pending_accounts(db: &Database, max_attempts: usize) -> Vec<AccountId> {
    db.read("accounts_with_pending_notes", move |tx| {
        accounts_with_pending_notes(tx, max_attempts)
    })
    .await
    .unwrap()
}

async fn do_insert_genesis(db: &Database, header: BlockHeader, commitment: Word) {
    db.write("insert_genesis_chain_state", move |tx| {
        insert_genesis_chain_state(tx, &header, &commitment)
    })
    .await
    .unwrap();
}

async fn do_update_tip(db: &Database, header: BlockHeader, mmr: PartialMmr) {
    let block_num = header.block_num();
    db.write("update_chain_state_tip", move |tx| {
        update_chain_state_tip(tx, block_num, &header, &mmr)
    })
    .await
    .unwrap();
}

async fn do_select_chain_state(db: &Database) -> Option<(BlockNumber, BlockHeader, PartialMmr)> {
    db.read("select_chain_state", select_chain_state).await.unwrap()
}

async fn do_lookup_script(db: &Database, root: Word) -> Option<NoteScript> {
    db.read("lookup_note_script", move |tx| lookup_note_script(tx, &root))
        .await
        .unwrap()
}

async fn do_insert_script(db: &Database, root: Word, script: NoteScript) {
    db.write("insert_note_script", move |tx| insert_note_script(tx, &root, &script))
        .await
        .unwrap();
}

async fn try_apply_block(
    db: &Database,
    effects: CommittedBlockEffects,
    mmr: PartialMmr,
) -> Result<(), DatabaseError> {
    db.write("apply_committed_block", move |tx| apply_committed_block(tx, &effects, &mmr))
        .await
}

// ACCOUNT UPSERT
// ================================================================================================

#[tokio::test]
async fn upsert_account_replaces_existing_row() {
    let (db, _dir) = test_setup().await;
    let account_id = mock_network_account_id();
    let account = mock_account(account_id);

    do_upsert_account(&db, account_id, account.clone(), mock_transaction_id(1)).await;
    do_upsert_account(&db, account_id, account, mock_transaction_id(2)).await;

    assert_eq!(count_accounts(&db).await, 1, "second upsert must overwrite, not insert");
    assert!(do_get_account(&db, account_id).await.is_some());
}

// NETWORK NOTE INSERT/DELETE
// ================================================================================================

#[tokio::test]
async fn insert_network_notes_is_idempotent() {
    let (db, _dir) = test_setup().await;
    let account_id = mock_network_account_id();
    let note = mock_single_target_note(account_id, 7);

    do_insert_notes(&db, vec![note.clone()]).await;
    // Re-applying the same block (e.g. on a subscription redelivery) must not error or duplicate.
    do_insert_notes(&db, vec![note]).await;

    assert_eq!(count_notes(&db).await, 1);
}

#[tokio::test]
async fn mark_notes_consumed_keeps_rows_and_sets_committed_at() {
    let (db, _dir) = test_setup().await;
    let account_id = mock_network_account_id();
    let note_a = mock_single_target_note(account_id, 1);
    let note_b = mock_single_target_note(account_id, 2);

    do_insert_notes(&db, vec![note_a.clone(), note_b.clone()]).await;
    assert_eq!(count_notes(&db).await, 2);

    let consumed_at = BlockNumber::from(42);
    do_mark_consumed(&db, vec![note_a.as_note().nullifier()], consumed_at).await;

    // Both rows are still present so the gRPC status endpoint can report them.
    assert_eq!(count_notes(&db).await, 2);

    let status_a = do_get_note_status(&db, note_a.as_note().id()).await.unwrap();
    assert_eq!(status_a.committed_at, Some(i64::from(consumed_at.as_u32())));

    let status_b = do_get_note_status(&db, note_b.as_note().id()).await.unwrap();
    assert!(status_b.committed_at.is_none());
}

#[tokio::test]
async fn mark_notes_consumed_is_noop_when_unknown() {
    let (db, _dir) = test_setup().await;
    let account_id = mock_network_account_id();
    let note = mock_single_target_note(account_id, 3);
    do_insert_notes(&db, vec![note.clone()]).await;

    // A nullifier we never inserted should not affect existing rows.
    let phantom = mock_single_target_note(account_id, 99).as_note().nullifier();
    do_mark_consumed(&db, vec![phantom], BlockNumber::from(5)).await;

    assert_eq!(count_notes(&db).await, 1);
    let status = do_get_note_status(&db, note.as_note().id()).await.unwrap();
    assert!(status.committed_at.is_none());
}

#[tokio::test]
async fn available_notes_excludes_consumed_notes() {
    let (db, _dir) = test_setup().await;
    let account_id = mock_network_account_id();
    let note = mock_single_target_note(account_id, 21);
    do_insert_notes(&db, vec![note.clone()]).await;

    assert_eq!(do_available_notes(&db, account_id, BlockNumber::from(1), 30).await.len(), 1);

    do_mark_consumed(&db, vec![note.as_note().nullifier()], BlockNumber::from(7)).await;

    assert!(
        do_available_notes(&db, account_id, BlockNumber::from(1000), 30)
            .await
            .is_empty()
    );
}

// AVAILABLE NOTES + BACKOFF
// ================================================================================================

#[tokio::test]
async fn available_notes_returns_unconsumed_under_attempt_cap() {
    let (db, _dir) = test_setup().await;
    let account_id = mock_network_account_id();
    let note = mock_single_target_note(account_id, 11);
    do_insert_notes(&db, vec![note]).await;

    let available = do_available_notes(&db, account_id, BlockNumber::from(1), 30).await;
    assert_eq!(available.len(), 1);
}

#[tokio::test]
async fn available_notes_excludes_attempts_at_cap() {
    let (db, _dir) = test_setup().await;
    let account_id = mock_network_account_id();
    let note = mock_single_target_note(account_id, 13);
    do_insert_notes(&db, vec![note.clone()]).await;

    // Push attempt_count up to the cap.
    let nullifier = note.as_note().nullifier();
    for _ in 0..30 {
        do_notes_failed(&db, vec![(nullifier, test_note_error("boom"))], BlockNumber::from(5))
            .await;
    }

    let available = do_available_notes(&db, account_id, BlockNumber::from(1000), 30).await;
    assert!(available.is_empty(), "notes at the attempt cap should not be available");
}

// CHAIN STATE
// ================================================================================================

#[tokio::test]
async fn update_chain_state_tip_persists_and_roundtrips_mmr() {
    let (db, _dir) = test_setup().await;
    let genesis = mock_block_header(BlockNumber::GENESIS);
    let header = mock_block_header(BlockNumber::from(7));
    let mmr = PartialMmr::default();

    do_insert_genesis(&db, genesis.clone(), genesis.commitment()).await;
    do_update_tip(&db, header.clone(), mmr).await;

    let (loaded_num, loaded_header, _loaded_mmr) = do_select_chain_state(&db).await.unwrap();
    assert_eq!(loaded_num, header.block_num());
    assert_eq!(loaded_header.block_num(), header.block_num());
}

#[tokio::test]
async fn update_chain_state_tip_keeps_singleton() {
    let (db, _dir) = test_setup().await;
    let genesis = mock_block_header(BlockNumber::GENESIS);
    let header_1 = mock_block_header(BlockNumber::from(1));
    let header_2 = mock_block_header(BlockNumber::from(2));
    let mmr = PartialMmr::default();

    do_insert_genesis(&db, genesis.clone(), genesis.commitment()).await;
    do_update_tip(&db, header_1, mmr.clone()).await;
    do_update_tip(&db, header_2.clone(), mmr).await;

    let (loaded_num, ..) = do_select_chain_state(&db).await.unwrap();
    assert_eq!(loaded_num, header_2.block_num());

    assert_eq!(count_chain_state(&db).await, 1, "chain_state must remain a singleton");
}

#[tokio::test]
async fn select_chain_state_returns_none_on_fresh_db() {
    let (db, _dir) = test_setup().await;
    assert!(do_select_chain_state(&db).await.is_none());
}

// NOTE SCRIPT CACHE
// ================================================================================================

#[tokio::test]
async fn note_script_cache_roundtrip() {
    let (db, _dir) = test_setup().await;
    let account_id = mock_network_account_id();
    let note = mock_single_target_note(account_id, 17);
    let script = note.as_note().script().clone();
    let root: Word = script.root().into();

    assert!(do_lookup_script(&db, root).await.is_none());
    do_insert_script(&db, root, script.clone()).await;
    assert!(do_lookup_script(&db, root).await.is_some());

    // Re-insert is idempotent.
    do_insert_script(&db, root, script).await;
}

// ACCOUNTS WITH PENDING NOTES
// ================================================================================================

#[tokio::test]
async fn accounts_with_pending_notes_distinct_and_filters_consumed_and_capped() {
    let (db, _dir) = test_setup().await;
    let alice = mock_network_account_id();
    let bob = mock_network_account_id_seeded(42);
    let carol = mock_network_account_id_seeded(99);

    let alice_note_1 = mock_single_target_note(alice, 1);
    let alice_note_2 = mock_single_target_note(alice, 2);
    let bob_note = mock_single_target_note(bob, 3);
    let carol_note = mock_single_target_note(carol, 4);

    do_insert_notes(&db, vec![alice_note_1, alice_note_2, bob_note.clone(), carol_note.clone()])
        .await;

    // Alice has two notes — must still appear exactly once (DISTINCT). Bob's only note is already
    // consumed — exclude.
    do_mark_consumed(&db, vec![bob_note.as_note().nullifier()], BlockNumber::from(7)).await;
    // Carol's note has hit the attempt cap — exclude.
    for _ in 0..30 {
        do_notes_failed(
            &db,
            vec![(carol_note.as_note().nullifier(), test_note_error("boom"))],
            BlockNumber::from(5),
        )
        .await;
    }

    let pending = do_pending_accounts(&db, 30).await;
    assert_eq!(pending.len(), 1, "only alice should remain pending");
    assert_eq!(pending[0], alice);
}

// SUBMITTED-TX LANDING
// ================================================================================================

#[tokio::test]
async fn account_last_tx_roundtrips_and_updates() {
    let (db, _dir) = test_setup().await;
    let account_id = mock_network_account_id();
    let account = mock_account(account_id);

    // The first upsert records its transaction id; a later upsert overwrites it.
    let first = mock_transaction_id(1);
    let second = mock_transaction_id(2);
    do_upsert_account(&db, account_id, account.clone(), first).await;
    assert_eq!(do_account_last_tx(&db, account_id).await, Some(first));
    do_upsert_account(&db, account_id, account, second).await;
    assert_eq!(do_account_last_tx(&db, account_id).await, Some(second));
}

#[tokio::test]
async fn account_last_tx_returns_none_for_untracked_account() {
    let (db, _dir) = test_setup().await;
    let account_id = mock_network_account_id();

    // No row exists for this account.
    assert_eq!(do_account_last_tx(&db, account_id).await, None);
}

// GENESIS APPLICATION
// ================================================================================================

/// Builds genesis-shaped effects: a full-state network-account update with no originating
/// transactions, at [`BlockNumber::GENESIS`].
fn genesis_effects() -> CommittedBlockEffects {
    let (account, details) = mock_network_account_update();
    CommittedBlockEffects {
        header: mock_block_header(BlockNumber::GENESIS),
        network_notes: vec![],
        nullifiers: vec![],
        network_account_updates: vec![(account.id(), details)],
        account_transactions: vec![],
    }
}

#[tokio::test]
async fn apply_committed_block_seeds_genesis_network_account() {
    let (db, _dir) = test_setup().await;
    let effects = genesis_effects();
    let account_id = effects.network_account_updates[0].0;

    // Genesis has no transactions, so this used to panic on the "must originate from a transaction"
    // invariant. It must now bootstrap the account successfully.
    try_apply_block(&db, effects, PartialMmr::default()).await.unwrap();

    assert!(
        do_get_account(&db, account_id).await.is_some(),
        "genesis account should be seeded"
    );
    // The seeded account carries the zero sentinel: no transaction produced it. An actor never
    // submits the zero id, so this can never be mistaken for a landed transaction.
    assert_eq!(
        do_account_last_tx(&db, account_id).await,
        Some(TransactionId::from_raw(Word::empty())),
    );
}

#[tokio::test]
async fn apply_committed_block_fails_on_txless_update_after_genesis() {
    let (db, _dir) = test_setup().await;
    // Same shape as genesis but at a non-genesis height: a committed account update with no
    // originating transaction is a real block-producer invariant violation. The
    // `apply_committed_block` assertion still fires; because the work runs on the pool's blocking
    // thread, the panic surfaces as an error rather than unwinding the test thread.
    let mut effects = genesis_effects();
    effects.header = mock_block_header(BlockNumber::from(1));

    try_apply_block(&db, effects, PartialMmr::default())
        .await
        .expect_err("a committed account update with no transaction must fail");
}

#[tokio::test]
async fn notes_failed_increments_attempt_and_records_error() {
    let (db, _dir) = test_setup().await;
    let account_id = mock_network_account_id();
    let note = mock_single_target_note(account_id, 19);
    do_insert_notes(&db, vec![note.clone()]).await;

    let nullifier = note.as_note().nullifier();
    do_notes_failed(&db, vec![(nullifier, test_note_error("nope"))], BlockNumber::from(5)).await;
    do_notes_failed(&db, vec![(nullifier, test_note_error("nope"))], BlockNumber::from(6)).await;

    let row = do_get_note_status(&db, note.as_note().id()).await.unwrap();
    assert_eq!(row.attempt_count, 2);
    assert_eq!(row.last_attempt, Some(6));
    assert!(row.last_error.is_some());
}

#[tokio::test]
async fn discard_notes_pins_attempts_to_cap_and_drops_from_pending() {
    let (db, _dir) = test_setup().await;
    let account_id = mock_network_account_id();
    let note = mock_single_target_note(account_id, 23);
    do_insert_notes(&db, vec![note.clone()]).await;

    let nullifier = note.as_note().nullifier();
    do_discard_notes(&db, vec![nullifier], BlockNumber::from(9), 30, "too big").await;

    // Pinned to the cap, so it is no longer pending or available for selection.
    let row = do_get_note_status(&db, note.as_note().id()).await.unwrap();
    assert_eq!(row.attempt_count, 30);
    assert_eq!(row.last_attempt, Some(9));
    assert_eq!(row.last_error.as_deref(), Some("too big"));

    assert!(
        do_available_notes(&db, account_id, BlockNumber::from(1000), 30)
            .await
            .is_empty(),
        "a discarded note must not be selectable",
    );
    assert!(
        !do_pending_accounts(&db, 30).await.contains(&account_id),
        "an account whose only note was discarded must not count as pending",
    );
}
