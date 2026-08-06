use std::path::Path;

use miden_node_db::DatabaseError;
use miden_node_utils::tracing::miden_instrument;

use crate::{COMPONENT, LOG_TARGET};

include!(concat!(env!("OUT_DIR"), "/db_migrator.rs"));

#[miden_instrument(
    level = "debug",
    target = COMPONENT,
    err,
)]
pub fn bootstrap_database(database_filepath: &Path) -> std::result::Result<(), DatabaseError> {
    let migrator = migrator().map_err(DatabaseError::migration)?;
    tracing::info!(
        target: LOG_TARGET,
        migration_count = migrator.schema_hashes().len(),
        "Bootstrapping database schema"
    );

    migrator.bootstrap(database_filepath).map_err(DatabaseError::migration)?;

    Ok(())
}

#[miden_instrument(
    level = "debug",
    target = COMPONENT,
    err,
)]
pub fn migrate_database(database_filepath: &Path) -> std::result::Result<(), DatabaseError> {
    let migrator = migrator().map_err(DatabaseError::migration)?;
    tracing::info!(
        target: LOG_TARGET,
        migration_count = migrator.schema_hashes().len(),
        "Applying database migrations"
    );

    migrator.migrate(database_filepath).map_err(DatabaseError::migration)?;

    Ok(())
}

#[miden_instrument(
    level = "debug",
    target = COMPONENT,
    err,
)]
pub fn verify_latest_schema(database_filepath: &Path) -> std::result::Result<(), DatabaseError> {
    let migrator = migrator().map_err(DatabaseError::migration)?;
    tracing::info!(
        target: LOG_TARGET,
        migration_count = migrator.schema_hashes().len(),
        "Verifying database schema"
    );

    migrator
        .verify_latest_schema(database_filepath)
        .map_err(DatabaseError::migration)?;

    Ok(())
}

#[cfg(test)]
pub(crate) fn test_connection() -> diesel::SqliteConnection {
    use diesel::{Connection, SqliteConnection};

    let temp_dir = tempfile::tempdir().expect("failed to create temp directory");
    let database_filepath = temp_dir.path().join("test.sqlite3");
    bootstrap_database(&database_filepath).expect("database should bootstrap");

    let conn = SqliteConnection::establish(
        database_filepath.to_str().expect("temp database path should be valid UTF-8"),
    )
    .expect("temp file sqlite should always work");
    let _kept_dir = temp_dir.keep();
    conn
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use anyhow::{Context, Result, ensure};
    use miden_node_db::migration::{SchemaHash, SchemaHashes};

    use super::*;

    const EXPECTED_SCHEMA_HASHES: [SchemaHash; 4] = [
        SchemaHash::from_hex("cc92cb332410e6f63036b52cf953acb446c142d5c0fbbdbd6d3b4f466510b210"),
        SchemaHash::from_hex("7c783947d0bb2c9745d28f4bdcf329f84ad970c36aa07ea85441e62718d8bbbb"),
        SchemaHash::from_hex("e026a70464e897ae9a217f45c80d72341b1bfb757200e57e41145348473a9961"),
        SchemaHash::from_hex("a581a13b00e4aa1d4539459e2b351c0585fad33c5a876f830c9b943adac92dea"),
    ];

    #[test]
    fn migration_schema_hashes_are_stable() -> Result<()> {
        let migrator = migrator()?;

        pretty_assertions::assert_eq!(
            migrator.schema_hashes(),
            SchemaHashes(&EXPECTED_SCHEMA_HASHES)
        );
        Ok(())
    }

    /// Builds a version-3 database with versioned `is_latest` rows and verifies that migration 4
    /// backfills each row's `valid_until` with its successor's `block_num` (or the open sentinel).
    #[test]
    fn validity_interval_migration_backfills_from_versioned_rows() -> Result<()> {
        use diesel::connection::SimpleConnection;
        use diesel::query_dsl::methods::{OrderDsl, SelectDsl};
        use diesel::{Connection, ExpressionMethods, RunQueryDsl, SqliteConnection};

        use crate::db::models::queries::VALID_FOREVER;
        use crate::db::schema;

        let temp_dir = tempfile::tempdir()?;
        let database_filepath = temp_dir.path().join("store.sqlite3");
        let database_path_str =
            database_filepath.to_str().context("database path should be valid UTF-8")?;

        {
            let mut conn = SqliteConnection::establish(database_path_str)?;
            conn.batch_execute(include_str!("migrations/001_initial.sql"))?;
            conn.batch_execute(include_str!("migrations/002_index_optimizations.sql"))?;
            conn.batch_execute(include_str!("migrations/003_block_headers_without_rowid.sql"))?;
            // One account updated at block 5, a vault key updated at block 5, a vault key written
            // once, and a storage-map key updated at block 5.
            conn.batch_execute(
                "INSERT INTO accounts \
                     (account_id, network_account_type, block_num, account_commitment, is_latest, \
                      created_at_block) \
                 VALUES (X'aa', 0, 1, X'01', 0, 1), (X'aa', 0, 5, X'02', 1, 1); \
                 INSERT INTO account_vault_assets \
                     (account_id, block_num, vault_key, asset, is_latest) \
                 VALUES (X'aa', 1, X'0b', X'01', 0), (X'aa', 5, X'0b', X'02', 1), \
                        (X'aa', 1, X'0c', X'03', 1); \
                 INSERT INTO account_storage_map_values \
                     (account_id, block_num, slot_name, key, value, is_latest) \
                 VALUES (X'aa', 1, 'slot', X'0d', X'01', 0), (X'aa', 5, 'slot', X'0d', X'02', 1); \
                 PRAGMA user_version = 3;",
            )?;
        }

        migrate_database(&database_filepath)?;

        let mut conn = SqliteConnection::establish(database_path_str)?;

        let accounts: Vec<(i64, i64)> = OrderDsl::order(
            SelectDsl::select(
                schema::accounts::table,
                (schema::accounts::block_num, schema::accounts::valid_until),
            ),
            schema::accounts::block_num.asc(),
        )
        .load(&mut conn)?;
        pretty_assertions::assert_eq!(accounts, vec![(1, 5), (5, VALID_FOREVER)]);

        let vault: Vec<(Vec<u8>, i64, i64)> = OrderDsl::order(
            SelectDsl::select(
                schema::account_vault_assets::table,
                (
                    schema::account_vault_assets::vault_key,
                    schema::account_vault_assets::block_num,
                    schema::account_vault_assets::valid_until,
                ),
            ),
            (
                schema::account_vault_assets::vault_key.asc(),
                schema::account_vault_assets::block_num.asc(),
            ),
        )
        .load(&mut conn)?;
        pretty_assertions::assert_eq!(
            vault,
            vec![
                (vec![0x0b], 1, 5),
                (vec![0x0b], 5, VALID_FOREVER),
                (vec![0x0c], 1, VALID_FOREVER),
            ]
        );

        let storage: Vec<(i64, i64)> = OrderDsl::order(
            SelectDsl::select(
                schema::account_storage_map_values::table,
                (
                    schema::account_storage_map_values::block_num,
                    schema::account_storage_map_values::valid_until,
                ),
            ),
            schema::account_storage_map_values::block_num.asc(),
        )
        .load(&mut conn)?;
        pretty_assertions::assert_eq!(storage, vec![(1, 5), (5, VALID_FOREVER)]);

        Ok(())
    }

    #[test]
    #[ignore = "requires diesel CLI; CI runs this in the diesel-schema job"]
    fn diesel_schema_is_in_sync_with_migrations() -> Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let database_filepath = temp_dir.path().join("store.sqlite3");
        bootstrap_database(&database_filepath)?;

        let output = Command::new("diesel")
            .arg("print-schema")
            .arg("--database-url")
            .arg(&database_filepath)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .context(
                "failed to run diesel CLI; install it with \
                 `cargo install diesel_cli --no-default-features --features sqlite`",
            )?;

        ensure!(
            output.status.success(),
            "diesel print-schema failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let generated =
            String::from_utf8(output.stdout).context("diesel CLI output is not UTF-8")?;
        assert_eq!(generated, include_str!("schema.rs"));
        Ok(())
    }
}
