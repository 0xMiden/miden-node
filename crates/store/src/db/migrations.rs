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

/// Bootstraps a throwaway database and returns its path.
///
/// The temporary directory is intentionally leaked so the file outlives the returned path; these are
/// test databases in the OS temp directory.
#[cfg(test)]
fn bootstrapped_test_database() -> std::path::PathBuf {
    let temp_dir = tempfile::tempdir().expect("failed to create temp directory");
    let database_filepath = temp_dir.path().join("test.sqlite3");
    bootstrap_database(&database_filepath).expect("database should bootstrap");
    let _kept_dir = temp_dir.keep();
    database_filepath
}

#[cfg(test)]
pub(crate) fn test_connection() -> diesel::SqliteConnection {
    use diesel::{Connection, SqliteConnection};

    let database_filepath = bootstrapped_test_database();
    SqliteConnection::establish(
        database_filepath.to_str().expect("temp database path should be valid UTF-8"),
    )
    .expect("temp file sqlite should always work")
}

/// Bootstraps a throwaway database and returns a framework connection to it.
///
/// The counterpart of [`test_connection`] for query functions that have moved onto the
/// `miden-node-db` SQLite framework; see [`TestConnection`](miden_node_db::sqlite::testing::TestConnection).
#[cfg(test)]
pub(crate) fn test_framework_connection() -> miden_node_db::sqlite::testing::TestConnection {
    miden_node_db::sqlite::testing::TestConnection::open(&bootstrapped_test_database())
        .expect("temp file sqlite should always work")
}

#[cfg(test)]
mod tests;
