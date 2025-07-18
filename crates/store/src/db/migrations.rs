use std::sync::LazyLock;

use miden_objects::crypto::hash::blake::{Blake3_160, Blake3Digest};
use rusqlite_migration::{M, Migrations, SchemaVersion};
use tracing::{debug, error, info, instrument};

use crate::{
    COMPONENT,
    db::{connection::Connection, settings::Settings, sql::utils::schema_version},
    errors::DatabaseError,
};

type Hash = Blake3Digest<20>;

const MIGRATION_SCRIPTS: [&str; 1] = [include_str!("migrations/001-init.sql")];
static MIGRATION_HASHES: LazyLock<Vec<Hash>> = LazyLock::new(compute_migration_hashes);
static MIGRATIONS: LazyLock<Migrations> = LazyLock::new(prepare_migrations);

fn up(s: &'static str) -> M<'static> {
    M::up(s).foreign_key_check()
}

const DB_MIGRATION_HASH_FIELD: &str = "db-migration-hash";
const DB_SCHEMA_VERSION_FIELD: &str = "db-schema-version";

#[instrument(level = "debug", target = COMPONENT, skip_all, err)]
pub fn apply_migrations(conn: &mut Connection) -> super::Result<()> {
    let version_before = MIGRATIONS.current_version(conn)?;

    info!(target: COMPONENT, %version_before, "Running database migrations");

    if let SchemaVersion::Inside(ver) = version_before {
        if !Settings::exists(conn)? {
            error!(target: COMPONENT, "No settings table in the database");
            return Err(DatabaseError::UnsupportedDatabaseVersion);
        }

        let last_schema_version: usize = Settings::get_value(conn, DB_SCHEMA_VERSION_FIELD)?
            .ok_or_else(|| {
                error!(target: COMPONENT, "No schema version in the settings table");
                DatabaseError::UnsupportedDatabaseVersion
            })?;
        let current_schema_version = schema_version(conn)?;

        if last_schema_version != current_schema_version {
            error!(target: COMPONENT, last_schema_version, current_schema_version, "Schema version mismatch");
            return Err(DatabaseError::UnsupportedDatabaseVersion);
        }

        let expected_hash = &*MIGRATION_HASHES[ver.get() - 1];
        let actual_hash = hex::decode(
            Settings::get_value::<String>(conn, DB_MIGRATION_HASH_FIELD)?
                .ok_or(DatabaseError::UnsupportedDatabaseVersion)?,
        )?;

        if actual_hash != expected_hash {
            error!(
                target: COMPONENT,
                expected_hash = hex::encode(expected_hash),
                actual_hash = hex::encode(&*actual_hash),
                "Migration hashes mismatch",
            );
            return Err(DatabaseError::UnsupportedDatabaseVersion);
        }
    }

    MIGRATIONS.to_latest(conn).map_err(DatabaseError::MigrationError)?;

    let version_after = MIGRATIONS.current_version(conn)?;

    if version_before != version_after {
        let new_hash = hex::encode(&*MIGRATION_HASHES[MIGRATION_HASHES.len() - 1]);
        debug!(target: COMPONENT, new_hash, "Updating migration hash in settings table");
        Settings::set_value(conn, DB_MIGRATION_HASH_FIELD, &new_hash)?;
    }

    info!(target: COMPONENT, "Starting database optimization");

    // Run full database optimization. This will run indexes analysis for the query planner.
    // This will also increase the `schema_version` value.
    //
    // We should run full database optimization in following cases:
    // 1. Once schema was changed, especially new indexes were created.
    // 2. After restarting of the node, on first connection established.
    //
    // More info: https://www.sqlite.org/pragma.html#pragma_optimize
    conn.pragma_update(None, "optimize", "0x10002")?;

    info!(target: COMPONENT, "Finished database optimization");

    let new_schema_version = schema_version(conn)?;
    debug!(target: COMPONENT, new_schema_version, "Updating schema version in settings table");
    Settings::set_value(conn, DB_SCHEMA_VERSION_FIELD, &new_schema_version)?;

    info!(target: COMPONENT, %version_after, "Finished database migrations");

    Ok(())
}

fn prepare_migrations() -> Migrations<'static> {
    Migrations::new(MIGRATION_SCRIPTS.map(up).to_vec())
}

fn compute_migration_hashes() -> Vec<Hash> {
    let mut accumulator = Hash::default();
    MIGRATION_SCRIPTS
        .iter()
        .map(|sql| {
            let script_hash = Blake3_160::hash(preprocess_sql(sql).as_bytes());
            accumulator = Blake3_160::merge(&[accumulator, script_hash]);
            accumulator
        })
        .collect()
}

// Strips new lines, whitespaces and comments.
fn preprocess_sql(sql: &str) -> String {
    sql.lines()
        .map(|line| line.split_once("--").unwrap_or((line, "")).0)
        .flat_map(|line| line.chars())
        .filter(|c| !c.is_whitespace())
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::db::migrations::{MIGRATIONS, preprocess_sql};

    #[test]
    fn migrations_validate() {
        assert_eq!(MIGRATIONS.validate(), Ok(()));
    }

    #[test]
    fn preprocess_sql_strips_unwanted_chars() {
        let whitespaces = "SELECT 1 FROM foo";
        assert_eq!(preprocess_sql(whitespaces), "SELECT1FROMfoo");
        let comments_and_new_lines = "SELECT 1; -- Comment\n-- Another comment\nSELECT 2";
        assert_eq!(preprocess_sql(comments_and_new_lines), "SELECT1;SELECT2");
    }
}
