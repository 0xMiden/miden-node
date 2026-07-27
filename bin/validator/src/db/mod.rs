mod migrations;

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::{Database, ReadTx, Row, WriteTx};
use miden_node_utils::tracing::miden_instrument;
use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::transaction::TransactionId;
use miden_protocol::utils::serde::Serializable;

use crate::db::migrations::{bootstrap_database, migrate_database, verify_latest_schema};
use crate::{
    COMPONENT,
    LOG_TARGET,
    PrivateRecordChainId,
    PrivateRecordContext,
    PrivateRecordId,
    PrivateRecordStorageFields,
    StorageKeyEpoch,
    StoredPrivateRecord,
};

/// SQL statements, kept in dedicated `.sql` files (under `sql/`).
mod sql {
    pub(super) const INSERT_TRANSACTION: &str = include_str!("sql/insert_transaction.sql");
    #[cfg(test)]
    pub(super) const LOAD_TRANSACTION: &str = include_str!("sql/load_transaction.sql");
    pub(super) const INSERT_PRIVATE_RECORD: &str = include_str!("sql/insert_private_record.sql");
    pub(super) const LOAD_PRIVATE_RECORD: &str = include_str!("sql/load_private_record.sql");
    pub(super) const LOAD_PRIVATE_RECORDS_BY_BLOCK_NUM: &str =
        include_str!("sql/load_private_records_by_block_num.sql");
    pub(super) const LOAD_PRIVATE_RECORDS_BY_KEY_EPOCH: &str =
        include_str!("sql/load_private_records_by_key_epoch.sql");
    pub(super) const LOAD_PRIVATE_RECORDS_BY_SETUP_CONTEXT: &str =
        include_str!("sql/load_private_records_by_setup_context.sql");
    pub(super) const LOAD_PRIVATE_RECORDS_BY_TRANSACTION: &str =
        include_str!("sql/load_private_records_by_transaction.sql");
    pub(super) const SET_PRIVATE_RECORD_BLOCK_NUM: &str =
        include_str!("sql/set_private_record_block_num.sql");
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

/// Inserts one encrypted private record.
#[miden_instrument(
    target = COMPONENT,
    skip_all,
    fields(
        record_id = ?record.context().record_id(),
        tx_id = %record.context().transaction_id(),
    ),
    err,
)]
pub fn insert_private_record(
    tx: &WriteTx<'_>,
    record: &StoredPrivateRecord,
) -> Result<usize, DatabaseError> {
    let context = record.context();
    let record_id = context.record_id().as_bytes().to_vec();
    let transaction_id = context.transaction_id().to_bytes();
    let chain_id = context.chain_id().as_bytes().to_vec();
    let key_epoch = context.key_epoch().as_bytes().to_vec();
    let setup_context_id = record.setup_context_id().to_vec();
    let schema_version = i64::from(context.schema_version());
    let block_num = record.block_num().map(|value| i64::from(value.as_u32()));
    let cipher_id = i64::from(record.cipher().id());
    let nonce = record.nonce().to_vec();
    let encrypted_record = record.encrypted_record().to_vec();
    let wrapped_content_key = record.wrapped_content_key().to_vec();

    tx.execute(
        sql::INSERT_PRIVATE_RECORD,
        &[
            &record_id,
            &transaction_id,
            &chain_id,
            &key_epoch,
            &setup_context_id,
            &schema_version,
            &block_num,
            &cipher_id,
            &nonce,
            &encrypted_record,
            &wrapped_content_key,
        ],
    )
}

/// Loads one encrypted private record by its stable record id.
#[miden_instrument(target = COMPONENT, skip_all, fields(?record_id), err)]
pub fn load_private_record(
    tx: &ReadTx<'_>,
    record_id: PrivateRecordId,
) -> Result<Option<StoredPrivateRecord>, DatabaseError> {
    Ok(tx
        .query(
            sql::LOAD_PRIVATE_RECORD,
            &[&record_id.as_bytes().to_vec()],
            private_record_from_row,
        )?
        .into_iter()
        .next())
}

/// Loads encrypted private records for one storage key epoch.
#[miden_instrument(target = COMPONENT, skip_all, fields(?key_epoch), err)]
pub fn load_private_records_by_key_epoch(
    tx: &ReadTx<'_>,
    key_epoch: StorageKeyEpoch,
) -> Result<Vec<StoredPrivateRecord>, DatabaseError> {
    tx.query(
        sql::LOAD_PRIVATE_RECORDS_BY_KEY_EPOCH,
        &[&key_epoch.as_bytes().to_vec()],
        private_record_from_row,
    )
}

/// Loads encrypted private records for one Golden setup context.
#[miden_instrument(target = COMPONENT, skip_all, fields(?setup_context_id), err)]
pub fn load_private_records_by_setup_context(
    tx: &ReadTx<'_>,
    setup_context_id: [u8; 32],
) -> Result<Vec<StoredPrivateRecord>, DatabaseError> {
    tx.query(
        sql::LOAD_PRIVATE_RECORDS_BY_SETUP_CONTEXT,
        &[&setup_context_id.to_vec()],
        private_record_from_row,
    )
}

/// Loads encrypted private records for one transaction.
#[miden_instrument(target = COMPONENT, skip_all, fields(%transaction_id), err)]
pub fn load_private_records_by_transaction(
    tx: &ReadTx<'_>,
    transaction_id: TransactionId,
) -> Result<Vec<StoredPrivateRecord>, DatabaseError> {
    tx.query(
        sql::LOAD_PRIVATE_RECORDS_BY_TRANSACTION,
        &[&transaction_id.to_bytes()],
        private_record_from_row,
    )
}

/// Loads encrypted private records included at one block number.
#[miden_instrument(target = COMPONENT, skip_all, fields(%block_num), err)]
pub fn load_private_records_by_block_num(
    tx: &ReadTx<'_>,
    block_num: BlockNumber,
) -> Result<Vec<StoredPrivateRecord>, DatabaseError> {
    tx.query(
        sql::LOAD_PRIVATE_RECORDS_BY_BLOCK_NUM,
        &[&i64::from(block_num.as_u32())],
        private_record_from_row,
    )
}

/// Adds block lookup data to every private record for one included transaction.
#[miden_instrument(
    target = COMPONENT,
    skip_all,
    fields(%transaction_id, %block_num),
    err,
)]
pub fn set_private_record_block_num(
    tx: &WriteTx<'_>,
    transaction_id: TransactionId,
    block_num: BlockNumber,
) -> Result<usize, DatabaseError> {
    tx.execute(
        sql::SET_PRIVATE_RECORD_BLOCK_NUM,
        &[&i64::from(block_num.as_u32()), &transaction_id.to_bytes()],
    )
}

fn private_record_from_row(row: &Row<'_>) -> Result<StoredPrivateRecord, DatabaseError> {
    let chain_id = fixed_32(row.get(0)?, "private record chain id")?;
    let key_epoch = fixed_32(row.get(1)?, "private record key epoch")?;
    let record_id = fixed_32(row.get(2)?, "private record id")?;
    let transaction_id = row.get(3)?;
    let setup_context_id = fixed_32(row.get(4)?, "private record setup context id")?;
    let schema_version = checked_u32(row.get(5)?, "private record schema version")?;
    let block_num = row
        .get::<Option<i64>>(6)?
        .map(|value| checked_u32(value, "private record block number").map(BlockNumber::from))
        .transpose()?;
    let cipher_id = checked_u16(row.get(7)?, "private record cipher id")?;
    let nonce = row.get(8)?;
    let encrypted_record = row.get(9)?;
    let wrapped_content_key = row.get(10)?;

    StoredPrivateRecord::from_storage_fields(PrivateRecordStorageFields {
        context: PrivateRecordContext::new(
            PrivateRecordChainId::new(chain_id),
            StorageKeyEpoch::new(key_epoch),
            PrivateRecordId::new(record_id),
            transaction_id,
        ),
        schema_version,
        setup_context_id,
        block_num,
        cipher_id,
        nonce,
        encrypted_record,
        wrapped_content_key,
    })
    .map_err(|source| DatabaseError::deserialization("private record", source))
}

fn fixed_32(bytes: Vec<u8>, field: &'static str) -> Result<[u8; 32], DatabaseError> {
    let actual_len = bytes.len();
    bytes.try_into().map_err(|_bytes: Vec<u8>| {
        let source = std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("expected 32 bytes, got {actual_len}"),
        );
        DatabaseError::deserialization(field, source)
    })
}

fn checked_u32(value: i64, field: &'static str) -> Result<u32, DatabaseError> {
    u32::try_from(value).map_err(|source| DatabaseError::deserialization(field, source))
}

fn checked_u16(value: i64, field: &'static str) -> Result<u16, DatabaseError> {
    u16::try_from(value).map_err(|source| DatabaseError::deserialization(field, source))
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
    use miden_protocol::Word;
    use rand_chacha_03::ChaCha20Rng;
    use rand_chacha_03::rand_core::SeedableRng;

    use super::*;
    use crate::private_record::test_private_record_sealer;

    const CHAIN_ID: PrivateRecordChainId = PrivateRecordChainId::new([1; 32]);
    const KEY_EPOCH: StorageKeyEpoch = StorageKeyEpoch::new([2; 32]);
    const RECORD_ID: PrivateRecordId = PrivateRecordId::new([3; 32]);
    const SETUP_CONTEXT_ID: [u8; 32] = [4; 32];

    fn private_record() -> StoredPrivateRecord {
        let transaction_id = TransactionId::from_raw(Word::from([5u32, 6, 7, 8]));
        let context = PrivateRecordContext::new(CHAIN_ID, KEY_EPOCH, RECORD_ID, transaction_id);
        let mut rng = ChaCha20Rng::from_seed([9; 32]);
        test_private_record_sealer(KEY_EPOCH, SETUP_CONTEXT_ID)
            .seal(&mut rng, context, b"private transaction inputs")
            .unwrap()
    }

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

    #[tokio::test]
    async fn private_record_indexes_work_before_and_after_inclusion() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp directory");
        let db = setup(temp_dir.path().join("validator.sqlite3")).await.unwrap();
        let record = private_record();
        let record_id = record.context().record_id();
        let transaction_id = record.context().transaction_id();

        let expected = record.clone();
        db.write("insert_private_record", move |tx| insert_private_record(tx, &record))
            .await
            .unwrap();

        let by_record = db
            .read("load_private_record", move |tx| load_private_record(tx, record_id))
            .await
            .unwrap();
        assert_eq!(by_record, Some(expected.clone()));

        let by_epoch = db
            .read("load_private_records_by_key_epoch", move |tx| {
                load_private_records_by_key_epoch(tx, KEY_EPOCH)
            })
            .await
            .unwrap();
        assert_eq!(by_epoch, vec![expected.clone()]);

        let by_setup = db
            .read("load_private_records_by_setup_context", move |tx| {
                load_private_records_by_setup_context(tx, SETUP_CONTEXT_ID)
            })
            .await
            .unwrap();
        assert_eq!(by_setup, vec![expected.clone()]);

        let by_transaction = db
            .read("load_private_records_by_transaction", move |tx| {
                load_private_records_by_transaction(tx, transaction_id)
            })
            .await
            .unwrap();
        assert_eq!(by_transaction, vec![expected.clone()]);

        let before_inclusion = db
            .read("load_private_records_by_block_num", move |tx| {
                load_private_records_by_block_num(tx, BlockNumber::from(12))
            })
            .await
            .unwrap();
        assert!(before_inclusion.is_empty());

        let updated = db
            .write("set_private_record_block_num", move |tx| {
                set_private_record_block_num(tx, transaction_id, BlockNumber::from(12))
            })
            .await
            .unwrap();
        assert_eq!(updated, 1);

        let after_inclusion = db
            .read("load_private_records_by_block_num", move |tx| {
                load_private_records_by_block_num(tx, BlockNumber::from(12))
            })
            .await
            .unwrap();
        let mut included = expected;
        included.set_block_num(BlockNumber::from(12));
        assert_eq!(after_inclusion, vec![included]);
    }

    #[tokio::test]
    async fn private_record_schema_has_required_indexes() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp directory");
        let db = setup(temp_dir.path().join("validator.sqlite3")).await.unwrap();

        let schema = db
            .read("private_record_schema", |tx| {
                tx.query(
                    "SELECT sql FROM sqlite_schema \
                     WHERE tbl_name = 'private_records' AND sql IS NOT NULL \
                     ORDER BY name",
                    &[],
                    |row| row.get::<String>(0),
                )
            })
            .await
            .unwrap()
            .join("\n");

        assert!(schema.contains("PRIMARY KEY (record_id)"));
        assert!(schema.contains("idx_private_records_key_epoch"));
        assert!(schema.contains("idx_private_records_setup_context_id"));
        assert!(schema.contains("idx_private_records_transaction_id"));
        assert!(schema.contains("idx_private_records_block_num"));
        assert!(schema.contains("WHERE block_num IS NOT NULL"));
    }
}
