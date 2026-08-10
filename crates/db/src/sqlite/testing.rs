//! A synchronous, single-connection handle for testing query functions.
//!
//! Query functions take a [`ReadTx`]/[`WriteTx`], which the async pool only ever hands out inside a
//! `read`/`write` closure. That is the right shape for production code, but it forces every test of
//! a query function to be `async` and to route through the owning component's database wrapper.

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use crate::DatabaseError;
use crate::sqlite::pool::configure_connection;
use crate::sqlite::tx::{ReadTx, WriteTx};

/// A single SQLite connection that hands out transaction handles synchronously, for tests.
///
/// The connection is configured identically to a pooled writer connection (WAL, foreign keys,
/// `busy_timeout`, statement cache, and the `array` module used by `rarray(?)` IN-lists), so query
/// functions behave here as they do in production.
pub struct TestConnection {
    conn: Connection,
}

impl TestConnection {
    /// Opens a connection to an existing database file.
    ///
    /// The caller is expected to have created the schema already (via its own migrator), just as the
    /// production `open` path expects a migrated file.
    pub fn open(database_filepath: &Path) -> Result<Self, DatabaseError> {
        let conn =
            Connection::open_with_flags(database_filepath, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        configure_connection(&conn, false)?;
        Ok(Self { conn })
    }

    /// Opens a connection to a private in-memory database.
    ///
    /// Useful for exercising the framework itself; component tests want [`open`](Self::open) so the
    /// schema comes from their migrations.
    pub fn open_in_memory() -> Result<Self, DatabaseError> {
        let conn = Connection::open_in_memory()?;
        configure_connection(&conn, false)?;
        Ok(Self { conn })
    }

    /// Returns a read handle. No transaction is opened; see the module docs.
    pub fn read(&self) -> ReadTx<'_> {
        ReadTx::new(&self.conn)
    }

    /// Returns a write handle. No transaction is opened, so each statement autocommits; see the
    /// module docs.
    pub fn write(&self) -> WriteTx<'_> {
        WriteTx::new(&self.conn)
    }

    /// Runs `work` inside a single `IMMEDIATE` transaction, committing if it returns `Ok` and
    /// rolling back otherwise.
    pub fn transact<T, E>(&self, work: impl FnOnce(&WriteTx<'_>) -> Result<T, E>) -> Result<T, E>
    where
        E: From<DatabaseError>,
    {
        self.conn.execute_batch("BEGIN IMMEDIATE").map_err(DatabaseError::from)?;
        match work(&self.write()) {
            Ok(value) => {
                self.conn.execute_batch("COMMIT").map_err(DatabaseError::from)?;
                Ok(value)
            },
            Err(err) => {
                // Preserve the original error: a failing rollback would only mask it.
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(err)
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates a single-column table to write to.
    fn setup() -> TestConnection {
        let db = TestConnection::open_in_memory().expect("in-memory database should open");
        db.conn
            .execute_batch("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
            .expect("table should be created");
        db
    }

    fn insert(db: &TestConnection, id: u32, name: &str) -> Result<usize, DatabaseError> {
        db.write()
            .execute("INSERT INTO items (id, name) VALUES (?1, ?2)", &[&id, &name])
    }

    fn names(db: &TestConnection) -> Vec<String> {
        db.read()
            .query("SELECT name FROM items ORDER BY id", &[], |row| row.get::<String>(0))
            .expect("select should succeed")
    }

    #[test]
    fn writes_autocommit_and_are_visible_to_the_next_read() {
        let db = setup();
        insert(&db, 1, "first").expect("insert should succeed");
        assert_eq!(names(&db), vec!["first".to_string()]);
    }

    #[test]
    fn transact_commits_on_ok() {
        let db = setup();
        db.transact::<_, DatabaseError>(|tx| {
            tx.execute("INSERT INTO items (id, name) VALUES (?1, ?2)", &[&1_u32, &"first"])?;
            tx.execute("INSERT INTO items (id, name) VALUES (?1, ?2)", &[&2_u32, &"second"])
        })
        .expect("transaction should commit");

        assert_eq!(names(&db), vec!["first".to_string(), "second".to_string()]);
    }

    #[test]
    fn transact_rolls_back_on_error() {
        let db = setup();
        let err = db
            .transact::<(), DatabaseError>(|tx| {
                tx.execute("INSERT INTO items (id, name) VALUES (?1, ?2)", &[&1_u32, &"first"])?;
                // Duplicate primary key: the whole transaction must roll back.
                tx.execute("INSERT INTO items (id, name) VALUES (?1, ?2)", &[&1_u32, &"again"])?;
                Ok(())
            })
            .expect_err("duplicate primary key should fail the transaction");

        assert_matches::assert_matches!(err, DatabaseError::Rusqlite(_));
        assert!(names(&db).is_empty(), "the successful insert must not have been committed");
    }

    #[test]
    fn read_handle_cannot_be_used_after_the_connection_is_dropped() {
        // A compile-time property rather than a runtime one, asserted here so it is not lost in a
        // later refactor: `read`/`write` borrow the connection, so no handle can outlive it.
        let db = setup();
        insert(&db, 1, "first").expect("insert should succeed");
        drop(db);
    }
}
