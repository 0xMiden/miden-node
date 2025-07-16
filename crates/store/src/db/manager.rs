//! A minimal connection manager wrapper
//!
//! Only required to setup connection parameters, specifically `WAL`.

use super::{DatabaseError, Result, RunQueryDsl, SqliteConnection};

pub(crate) struct WalConnManager {
    pub(crate) manager: deadpool_diesel::sqlite::Manager,
}

impl WalConnManager {
    pub(crate) fn new(database_path: &str) -> Self {
        let manager = deadpool_diesel::sqlite::Manager::new(
            database_path.to_owned(),
            deadpool_diesel::sqlite::Runtime::Tokio1,
        );
        Self { manager }
    }
}

// FIXME use a better error type
impl deadpool::managed::Manager for WalConnManager {
    type Type = deadpool_sync::SyncWrapper<SqliteConnection>;
    type Error = DatabaseError;

    async fn create(&self) -> Result<Self::Type, Self::Error> {
        let conn = self.manager.create().await?;

        conn.interact(configure_connection_on_creation)
            .await
            .map_err(|e| DatabaseError::interact("Connection setup", &e))??;
        Ok(conn)
    }

    async fn recycle(
        &self,
        conn: &mut Self::Type,
        metrics: &deadpool_diesel::Metrics,
    ) -> deadpool::managed::RecycleResult<Self::Error> {
        self.manager.recycle(conn, metrics).await.map_err(DatabaseError::PoolRecycle)?;
        Ok(())
    }
}

pub(crate) fn configure_connection_on_creation(
    conn: &mut SqliteConnection,
) -> Result<(), DatabaseError> {
    // Enable the WAL mode. This allows concurrent reads while the
    // transaction is being written, this is required for proper
    // synchronization of the servers in-memory and on-disk representations
    // (see [State::apply_block])
    diesel::sql_query("PRAGMA journal_mode=WAL").execute(conn)?;

    // Enable foreign key checks.
    diesel::sql_query("PRAGMA foreign_keys=ON").execute(conn)?;
    Ok(())
}
