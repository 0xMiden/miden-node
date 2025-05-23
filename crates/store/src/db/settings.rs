use rusqlite::{OptionalExtension, Result, ToSql, params, types::FromSql};

use crate::db::{connection::Connection, sql::utils::table_exists};

pub struct Settings;

impl Settings {
    pub fn exists(conn: &mut Connection) -> Result<bool> {
        table_exists(&conn.transaction()?, "settings")
    }

    pub fn get_value<T: FromSql>(conn: &mut Connection, name: &str) -> Result<Option<T>> {
        conn.transaction()?
            .query_row("SELECT value FROM settings WHERE name = $1", params![name], |row| {
                row.get(0)
            })
            .optional()
    }

    pub fn set_value<T: ToSql>(conn: &Connection, name: &str, value: &T) -> Result<()> {
        let count =
            conn.execute(insert_sql!(settings { name, value } | REPLACE), params![name, value])?;

        debug_assert_eq!(count, 1);

        Ok(())
    }
}
