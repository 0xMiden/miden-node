use diesel::SqliteConnection;
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use tracing::instrument;

use crate::COMPONENT;
pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("src/db/migrations");

// TODO we have not tested this in practice!
#[instrument(level = "debug", target = COMPONENT, skip_all, err)]
pub fn apply_migrations(
    conn: &mut SqliteConnection,
) -> std::result::Result<(), crate::errors::DatabaseError> {
    let migrations = conn.pending_migrations(MIGRATIONS).expect("In memory migrations never fail");
    println!("Applying migrations: {}", migrations.len());

    let Err(e) = conn.run_pending_migrations(MIGRATIONS) else {
        return Ok(());
    };
    eprintln!("Failed to apply migration: {e:?}");
    // something went wrong, MIGRATIONS contains
    conn.revert_last_migration(MIGRATIONS)
        .expect("Duality is maintained by the developer");

    Ok(())
}
