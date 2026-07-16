use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use anyhow::Context;
use miden_node_db::DatabaseError;
use miden_node_db::sqlite::Database;
use miden_node_utils::tracing::miden_instrument;
use miden_protocol::block::SignedBlock;
use miden_protocol::crypto::merkle::mmr::PartialMmr;
use tracing::info;

use crate::COMPONENT;
use crate::committed_block::CommittedBlockEffects;
use crate::db::migrations::{bootstrap_database, migrate_database, verify_latest_schema};

pub(crate) mod queries;

mod migrations;

/// SQL statements, kept in dedicated `.sql` files (under `sql/`).
pub(crate) mod sql {
    pub(crate) const UPSERT_ACCOUNT: &str = include_str!("sql/upsert_account.sql");
    pub(crate) const ACCOUNT_LAST_TX: &str = include_str!("sql/account_last_tx.sql");
    pub(crate) const ACCOUNT_EXISTS: &str = include_str!("sql/account_exists.sql");
    pub(crate) const GET_ACCOUNT: &str = include_str!("sql/get_account.sql");
    pub(crate) const UPDATE_CHAIN_STATE_TIP: &str = include_str!("sql/update_chain_state_tip.sql");
    pub(crate) const INSERT_GENESIS_CHAIN_STATE: &str =
        include_str!("sql/insert_genesis_chain_state.sql");
    pub(crate) const SELECT_GENESIS_COMMITMENT: &str =
        include_str!("sql/select_genesis_commitment.sql");
    pub(crate) const SELECT_CHAIN_STATE: &str = include_str!("sql/select_chain_state.sql");
    pub(crate) const LOOKUP_NOTE_SCRIPT: &str = include_str!("sql/lookup_note_script.sql");
    pub(crate) const INSERT_NOTE_SCRIPT: &str = include_str!("sql/insert_note_script.sql");
    pub(crate) const INSERT_NETWORK_NOTE: &str = include_str!("sql/insert_network_note.sql");
    pub(crate) const MARK_NOTE_CONSUMED: &str = include_str!("sql/mark_note_consumed.sql");
    pub(crate) const AVAILABLE_NOTES: &str = include_str!("sql/available_notes.sql");
    pub(crate) const NOTE_FAILED: &str = include_str!("sql/note_failed.sql");
    pub(crate) const DISCARD_NOTE: &str = include_str!("sql/discard_note.sql");
    pub(crate) const GET_NOTE_STATUS: &str = include_str!("sql/get_note_status.sql");
    pub(crate) const ACCOUNTS_WITH_PENDING_NOTES: &str =
        include_str!("sql/accounts_with_pending_notes.sql");
}

// LIFECYCLE
// ================================================================================================

/// Opens an async connection pool after verifying the database is at the latest schema version.
#[miden_instrument(
    target = COMPONENT,
    name = "ntx_builder.database.load",
    skip_all,
    fields(path=%database_filepath.display()),
    err,
)]
pub async fn load(database_filepath: PathBuf) -> anyhow::Result<Database> {
    load_with_pool_size(database_filepath, miden_node_db::default_connection_pool_size()).await
}

/// Opens an async connection pool with a specific pool size after verifying the database is at the
/// latest schema version.
#[miden_instrument(
    target = COMPONENT,
    name = "ntx_builder.database.load",
    skip_all,
    fields(path=%database_filepath.display()),
    err,
)]
pub async fn load_with_pool_size(
    database_filepath: PathBuf,
    connection_pool_size: NonZeroUsize,
) -> anyhow::Result<Database> {
    verify_latest_schema(&database_filepath).context("failed to verify database schema")?;

    open_with_pool_size(&database_filepath, connection_pool_size)
}

/// Applies all pending migrations to an existing DB.
#[miden_instrument(target = COMPONENT, skip_all)]
pub fn migrate(database_filepath: impl AsRef<Path>) -> Result<(), DatabaseError> {
    migrate_database(database_filepath.as_ref())?;
    Ok(())
}

fn open_with_pool_size(
    database_filepath: &Path,
    connection_pool_size: NonZeroUsize,
) -> anyhow::Result<Database> {
    let db = Database::new_with_pool_size(database_filepath, connection_pool_size)
        .context("failed to build connection pool")?;

    info!(
        target: COMPONENT,
        sqlite = %database_filepath.display(),
        connection_pool_size = %connection_pool_size,
        "Connected to the database"
    );

    Ok(db)
}

/// Creates and initializes the database, then seeds it with the signed genesis block.
///
/// Mirrors the store's bootstrap: after this completes the singleton `chain_state` row exists at
/// [`BlockNumber::GENESIS`](miden_protocol::block::BlockNumber::GENESIS), so
/// [`crate::NtxBuilderConfig::build`] can assume the genesis block is always present and never has
/// to consume it from the committed-block subscription on startup.
///
/// Returns an error if the database has already been bootstrapped.
#[miden_instrument(
    target = COMPONENT,
    name = "ntx_builder.database.bootstrap",
    skip_all,
    fields(path=%database_filepath.display()),
    err,
)]
pub async fn bootstrap(database_filepath: PathBuf, genesis: &SignedBlock) -> anyhow::Result<()> {
    bootstrap_database(&database_filepath).context("failed to bootstrap database schema")?;
    let db =
        open_with_pool_size(&database_filepath, miden_node_db::default_connection_pool_size())?;

    let genesis_commitment = genesis.header().commitment();
    let genesis_header = genesis.header().clone();

    db.write("insert_genesis_chain_state", move |tx| {
        queries::insert_genesis_chain_state(tx, &genesis_header, &genesis_commitment)
    })
    .await
    .context("failed to seed genesis chain state")?;

    let effects = CommittedBlockEffects::from_signed_block(genesis);
    db.write("apply_committed_block", move |tx| {
        queries::apply_committed_block(tx, &effects, &PartialMmr::default())
    })
    .await
    .context("failed to insert genesis block")?;

    Ok(())
}

// TEST HELPERS
// ================================================================================================

/// Creates a schema-migrated (but un-seeded) database backed by a temp file for testing.
#[cfg(test)]
pub(crate) async fn test_setup() -> (Database, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("failed to create temp directory");
    let db_path = dir.path().join("test.sqlite3");
    bootstrap_database(&db_path).expect("database should bootstrap");
    let db = load(db_path).await.expect("test DB load should succeed");
    (db, dir)
}

/// Seeds a committed account row (and its `last_tx_id`) for tests that exercise the actor's landing
/// detection without driving a full committed block.
#[cfg(test)]
pub(crate) async fn upsert_account_for_test(
    db: &Database,
    account_id: miden_protocol::account::AccountId,
    account: miden_protocol::account::Account,
    last_tx_id: miden_protocol::transaction::TransactionId,
) -> Result<(), DatabaseError> {
    db.write("test_upsert_account", move |tx| {
        queries::upsert_account(tx, account_id, &account, last_tx_id)
    })
    .await
}

#[cfg(test)]
mod tests {
    use miden_protocol::block::BlockNumber;

    use super::*;
    use crate::db::queries;
    use crate::test_utils::{mock_genesis_block, mock_genesis_block_with_network_account};

    #[tokio::test]
    async fn bootstrap_seeds_genesis_network_account() {
        let dir = tempfile::tempdir().expect("failed to create temp directory");
        let db_path = dir.path().join("ntx-builder.sqlite3");

        let (genesis, account_id) = mock_genesis_block_with_network_account();
        bootstrap(db_path.clone(), &genesis)
            .await
            .expect("bootstrap should succeed with a network account in genesis");

        let db = load(db_path).await.expect("load should open the bootstrapped database");
        let account = db
            .read("get_account", move |tx| queries::get_account(tx, account_id))
            .await
            .expect("query should succeed");
        assert!(account.is_some(), "genesis network account should be committed after bootstrap");
    }

    #[tokio::test]
    async fn bootstrap_seeds_genesis_chain_state() {
        let dir = tempfile::tempdir().expect("failed to create temp directory");
        let db_path = dir.path().join("ntx-builder.sqlite3");

        bootstrap(db_path.clone(), &mock_genesis_block())
            .await
            .expect("bootstrap should succeed on a fresh database");

        let db = load(db_path).await.expect("load should open the bootstrapped database");
        let (block_num, ..) = db
            .read("select_chain_state", queries::select_chain_state)
            .await
            .expect("query should succeed")
            .expect("chain state should be present after bootstrap");

        assert_eq!(block_num, BlockNumber::GENESIS);
    }

    #[tokio::test]
    async fn bootstrap_rejects_already_bootstrapped_database() {
        let dir = tempfile::tempdir().expect("failed to create temp directory");
        let db_path = dir.path().join("ntx-builder.sqlite3");

        bootstrap(db_path.clone(), &mock_genesis_block())
            .await
            .expect("first bootstrap should succeed");

        let err = bootstrap(db_path, &mock_genesis_block())
            .await
            .expect_err("second bootstrap should fail");
        assert!(
            err.chain().any(|source| source.to_string().contains("database already exists")),
            "unexpected error: {err}"
        );
    }
}
