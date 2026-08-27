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
mod tests;
