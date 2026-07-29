mod migrations;

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::{Database, ReadTx, WriteTx};
use miden_node_utils::tracing::miden_instrument;
use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::transaction::TransactionId;
use miden_protocol::utils::serde::Serializable;

use crate::db::migrations::{bootstrap_database, migrate_database, verify_latest_schema};
use crate::{COMPONENT, LOG_TARGET};

/// SQL statements, kept in dedicated `.sql` files (under `sql/`).
mod sql {
    pub(super) const INSERT_TRANSACTION: &str = include_str!("sql/insert_transaction.sql");
    #[cfg(test)]
    pub(super) const LOAD_TRANSACTION: &str = include_str!("sql/load_transaction.sql");
    pub(super) const TRANSACTION_EXISTS: &str = include_str!("sql/transaction_exists.sql");
    pub(super) const UPSERT_BLOCK_HEADER: &str = include_str!("sql/upsert_block_header.sql");
    pub(super) const LOAD_CHAIN_TIP: &str = include_str!("sql/load_chain_tip.sql");
    pub(super) const LOAD_BLOCK_HEADER: &str = include_str!("sql/load_block_header.sql");
    pub(super) const COUNT_VALIDATED_TRANSACTIONS: &str =
        include_str!("sql/count_validated_transactions.sql");
    pub(super) const COUNT_SIGNED_BLOCKS: &str = include_str!("sql/count_signed_blocks.sql");
}

/// Open a connection to the DB after verifying that it is at the latest schema version.
#[miden_instrument(
    target = COMPONENT,
    skip_all,
)]
pub async fn load(database_filepath: PathBuf) -> Result<Database, DatabaseError> {
    load_with_pool_size(database_filepath, miden_node_db::default_connection_pool_size()).await
}

/// Open a connection to the DB with a specific pool size after verifying that it is at the latest
/// schema version.
#[miden_instrument(
    target = COMPONENT,
    skip_all,
)]
pub async fn load_with_pool_size(
    database_filepath: PathBuf,
    connection_pool_size: NonZeroUsize,
) -> Result<Database, DatabaseError> {
    verify_latest_schema(&database_filepath)?;

    open_with_pool_size(&database_filepath, connection_pool_size)
}

/// Creates a new database, applies all migrations, and opens a connection pool.
#[miden_instrument(
    target = COMPONENT,
    skip_all,
)]
pub async fn setup(database_filepath: PathBuf) -> Result<Database, DatabaseError> {
    setup_with_pool_size(database_filepath, miden_node_db::default_connection_pool_size()).await
}

/// Creates a new database with a specific pool size and applies all migrations.
#[miden_instrument(
    target = COMPONENT,
    skip_all,
)]
pub async fn setup_with_pool_size(
    database_filepath: PathBuf,
    connection_pool_size: NonZeroUsize,
) -> Result<Database, DatabaseError> {
    bootstrap_database(&database_filepath)?;

    open_with_pool_size(&database_filepath, connection_pool_size)
}

/// Applies all pending migrations to an existing DB.
#[miden_instrument(
    target = COMPONENT,
    skip_all,
)]
pub fn migrate(database_filepath: impl AsRef<Path>) -> Result<(), DatabaseError> {
    migrate_database(database_filepath.as_ref())?;
    Ok(())
}

fn open_with_pool_size(
    database_filepath: &Path,
    connection_pool_size: NonZeroUsize,
) -> Result<Database, DatabaseError> {
    let db = Database::new_with_pool_size(database_filepath, connection_pool_size)?;
    tracing::info!(
        target: LOG_TARGET,
        sqlite= %database_filepath.display(),
        connection_pool_size = %connection_pool_size,
        "Connected to the database"
    );
    Ok(db)
}

/// The sealed transaction inputs accepted by the validator.
///
/// This is the Phase 1 storage record. Phase 2 will replace the client envelope with inputs
/// re-encrypted under a fresh content key protected by Golden EHTDH1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedTransactionRecord {
    pub transaction_id: TransactionId,
    pub submission_scheme: u32,
    pub submission_key_id: Vec<u8>,
    pub sealed_transaction_inputs: Vec<u8>,
}

/// Inserts the accepted sealed inputs and validated marker in one database write.
#[miden_instrument(
    target = COMPONENT,
    skip_all,
    fields(
        transaction.id = %record.transaction_id,
    ),
    err,
)]
pub(crate) fn insert_transaction(
    tx: &WriteTx<'_>,
    record: &ValidatedTransactionRecord,
) -> Result<usize, DatabaseError> {
    let id = record.transaction_id.to_bytes();
    let submission_scheme = i64::from(record.submission_scheme);

    tx.execute(
        sql::INSERT_TRANSACTION,
        &[
            &id,
            &submission_scheme,
            &record.submission_key_id,
            &record.sealed_transaction_inputs,
        ],
    )
}

/// Loads the sealed record stored for a validated transaction.
#[cfg(test)]
pub(crate) fn load_transaction(
    tx: &ReadTx<'_>,
    tx_id: TransactionId,
) -> Result<Option<ValidatedTransactionRecord>, DatabaseError> {
    tx.query(sql::LOAD_TRANSACTION, &[&tx_id.to_bytes()], |row| {
        let submission_scheme = row
            .get::<i64>(0)?
            .try_into()
            .expect("stored submission scheme should fit in u32");
        Ok(ValidatedTransactionRecord {
            transaction_id: tx_id,
            submission_scheme,
            submission_key_id: row.get(1)?,
            sealed_transaction_inputs: row.get(2)?,
        })
    })
    .map(|mut records| records.pop())
}

/// Returns whether a transaction with the given id has already been validated.
#[miden_instrument(
    target = COMPONENT,
    skip(tx),
    err,
)]
pub(crate) fn transaction_exists(
    tx: &ReadTx<'_>,
    tx_id: TransactionId,
) -> Result<bool, DatabaseError> {
    let exists = tx
        .query(sql::TRANSACTION_EXISTS, &[&tx_id.to_bytes()], |row| row.get::<i64>(0))?
        .first()
        .copied()
        .unwrap_or(0)
        != 0;
    Ok(exists)
}

/// Scans the database for transaction Ids that do not exist.
///
/// If the resulting vector is empty, all supplied transaction ids have been validated in the past.
#[miden_instrument(
    target = COMPONENT,
    skip(tx),
    err,
)]
pub(crate) fn find_unvalidated_transactions(
    tx: &ReadTx<'_>,
    tx_ids: &[TransactionId],
) -> Result<Vec<TransactionId>, DatabaseError> {
    let mut unvalidated_tx_ids = Vec::new();
    for tx_id in tx_ids {
        // Check whether each transaction id exists in the database.
        let exists = tx
            .query(sql::TRANSACTION_EXISTS, &[&tx_id.to_bytes()], |row| row.get::<i64>(0))?
            .first()
            .copied()
            .unwrap_or(0)
            != 0;
        // Record any transaction ids that do not exist.
        if !exists {
            unvalidated_tx_ids.push(*tx_id);
        }
    }
    Ok(unvalidated_tx_ids)
}

/// Upserts a block header into the database.
///
/// Inserts a new row if no block header exists at the given block number, or replaces the
/// existing block header if one already exists.
#[miden_instrument(
    target = COMPONENT,
    skip(tx, header),
    err,
)]
pub fn upsert_block_header(tx: &WriteTx<'_>, header: &BlockHeader) -> Result<(), DatabaseError> {
    let block_num = i64::from(header.block_num().as_u32());
    let block_header = header.to_bytes();
    tx.execute(sql::UPSERT_BLOCK_HEADER, &[&block_num, &block_header])?;
    Ok(())
}

/// Loads the chain tip (block header with the highest block number) from the database.
///
/// Returns `None` if no block headers have been persisted (i.e. bootstrap has not been run).
#[miden_instrument(
    target = COMPONENT,
    skip(tx),
    err,
)]
pub fn load_chain_tip(tx: &ReadTx<'_>) -> Result<Option<BlockHeader>, DatabaseError> {
    Ok(tx
        .query(sql::LOAD_CHAIN_TIP, &[], |row| row.get::<BlockHeader>(0))?
        .into_iter()
        .next())
}

/// Loads a block header by its block number.
///
/// Returns `None` if no block header exists at the given block number.
#[miden_instrument(
    target = COMPONENT,
    skip(tx),
    err,
)]
pub fn load_block_header(
    tx: &ReadTx<'_>,
    block_num: BlockNumber,
) -> Result<Option<BlockHeader>, DatabaseError> {
    Ok(tx
        .query(sql::LOAD_BLOCK_HEADER, &[&i64::from(block_num.as_u32())], |row| {
            row.get::<BlockHeader>(0)
        })?
        .into_iter()
        .next())
}

/// Returns the total number of validated transactions in the database.
#[miden_instrument(
    target = COMPONENT,
    skip(tx),
    err,
)]
pub fn count_validated_transactions(tx: &ReadTx<'_>) -> Result<i64, DatabaseError> {
    Ok(tx
        .query(sql::COUNT_VALIDATED_TRANSACTIONS, &[], |row| row.get::<i64>(0))?
        .into_iter()
        .next()
        .unwrap_or(0))
}

/// Returns the total number of signed blocks in the database.
#[miden_instrument(
    target = COMPONENT,
    skip(tx),
    err,
)]
pub fn count_signed_blocks(tx: &ReadTx<'_>) -> Result<i64, DatabaseError> {
    Ok(tx
        .query(sql::COUNT_SIGNED_BLOCKS, &[], |row| row.get::<i64>(0))?
        .into_iter()
        .next()
        .unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_rejects_missing_database() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp directory");
        let db_path = temp_dir.path().join("validator.sqlite3");

        let err = migrate(db_path.clone()).expect_err("missing database should fail");

        assert!(matches!(err, DatabaseError::Migration(_)), "unexpected error: {err:?}");
        assert!(!db_path.exists());
    }

    #[tokio::test]
    async fn setup_creates_database_that_load_accepts() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp directory");
        let db_path = temp_dir.path().join("validator.sqlite3");

        setup(db_path.clone()).await.expect("setup should bootstrap the database");
        load(db_path).await.expect("load should accept a bootstrapped database");
    }

    #[tokio::test]
    async fn transaction_exists_detects_validated_transactions() {
        use miden_protocol::Word;

        let temp_dir = tempfile::tempdir().expect("failed to create temp directory");
        let db = setup(temp_dir.path().join("validator.sqlite3")).await.unwrap();

        let validated_id = TransactionId::from_raw(Word::try_from([1u64, 2, 3, 4]).unwrap());
        let unknown_id = TransactionId::from_raw(Word::try_from([5u64, 6, 7, 8]).unwrap());

        // Insert a row keyed by `validated_id`.
        let id = validated_id.to_bytes();
        let empty: Vec<u8> = vec![];
        db.write("insert_row", move |tx| {
            tx.execute(
                "INSERT INTO validated_transactions \
                 (id, submission_scheme, submission_key_id, sealed_transaction_inputs) \
                 VALUES (?1, ?2, ?3, ?4)",
                &[&id, &1i64, &empty, &empty],
            )
        })
        .await
        .unwrap();

        let validated_exists = db
            .read("transaction_exists", move |tx| transaction_exists(tx, validated_id))
            .await
            .unwrap();
        assert!(validated_exists, "an inserted transaction id should be reported as existing");

        let unknown_exists = db
            .read("transaction_exists", move |tx| transaction_exists(tx, unknown_id))
            .await
            .unwrap();
        assert!(!unknown_exists, "an unknown transaction id should not be reported as existing");
    }
}
