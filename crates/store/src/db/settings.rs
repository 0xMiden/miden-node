// TODO figure out if we want to retain the settings
#![allow(dead_code)]

use diesel::{
    ExpressionMethods, OptionalExtension, QueryDsl, RunQueryDsl, SqliteConnection,
    query_dsl::methods::SelectDsl,
};
use miden_lib::utils::Deserializable;

use crate::{
    db::{models::table_exists, schema},
    errors::DatabaseError,
};

pub struct Settings;

impl Settings {
    pub fn exists(conn: &mut SqliteConnection) -> Result<bool, DatabaseError> {
        table_exists(conn, "settings")
    }

    pub fn get_value<T: Deserializable>(
        conn: &mut SqliteConnection,
        name: &str,
    ) -> Result<Option<T>, DatabaseError> {
        let maybe_serialized: Option<Option<Vec<u8>>> =
            SelectDsl::select(schema::settings::table, schema::settings::value)
                .filter(schema::settings::name.eq(name))
                .get_result(conn)
                .optional()?;
        Ok(maybe_serialized
            .flatten()
            .as_ref()
            .map(|x| T::read_from_bytes(x.as_slice()))
            .transpose()?)
    }

    pub fn set_value<T: AsRef<[u8]>>(
        conn: &mut SqliteConnection,
        name: &str,
        value: &T,
    ) -> Result<(), DatabaseError> {
        let value: &[u8] = value.as_ref();
        let val = (schema::settings::name.eq(name), schema::settings::value.eq(value.to_vec()));
        let count = diesel::insert_into(schema::settings::table)
            .values(&val)
            .on_conflict(schema::settings::name)
            .do_update()
            .set(val.clone())
            .execute(conn)?;

        debug_assert_eq!(count, 1);

        Ok(())
    }
}
