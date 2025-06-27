use miden_lib::utils::{Deserializable, DeserializationError, Serializable};
use miden_objects::block::BlockNumber;

/// Utility to convert an iterable container of containing `R`-typed values
/// to a `Vec<D>` and bail at the first failing conversion
pub(crate) fn vec_raw_try_into<D, R: TryInto<D>>(
    raw: impl IntoIterator<Item = R>,
) -> std::result::Result<Vec<D>, <R as TryInto<D>>::Error> {
    std::result::Result::<Vec<D>, <R as TryInto<D>>::Error>::from_iter(
        raw.into_iter().map(<R as std::convert::TryInto<D>>::try_into),
    )
}

#[allow(dead_code)]
/// Deserialize an iterable container full of byte blobs `B` to types `T`
pub(crate) fn deserialize_raw_vec<B: AsRef<[u8]>, T: Deserializable>(
    raw: impl IntoIterator<Item = B>,
) -> std::result::Result<Vec<T>, DeserializationError> {
    std::result::Result::<Vec<_>, DeserializationError>::from_iter(
        raw.into_iter().map(|raw| T::read_from_bytes(raw.as_ref())),
    )
}

/// Utility to convert an iterable container to a vector of byte blobs
pub(crate) fn serialize_vec<'a, D: Serializable + 'a>(
    raw: impl IntoIterator<Item = &'a D>,
) -> Vec<Vec<u8>> {
    Vec::<_>::from_iter(raw.into_iter().map(<D as Serializable>::to_bytes))
}

// TODO once all integers are wrapper types, use something like
// trait FlatAndBloat {
//     type Raw = i64;
//     fn to_raw_sql(self) -> Self::Raw;
//     fn from_raw_sql(Self::Raw) -> Self;
// }

/// Convert the database type `BigInt` into a blocknumber
///
/// Attention: We use `u32` as actual in-memory representation, since this will
/// suffice well beyond all our lifetimes.
pub(crate) fn raw_sql_to_block_number(raw: impl Into<i64>) -> BlockNumber {
    let raw = raw.into();
    debug_assert!(raw <= u32::MAX as i64);
    #[allow(clippy::cast_sign_loss)]
    BlockNumber::from(raw as u32)
}

/// Convert the in-memory type `BlockNumber` into the database type `BigInt`
///
/// Attention: We use `u32` as actual in-memory representation, since this will
/// suffice well beyond all our lifetimes.
pub(crate) fn block_number_to_raw_sql(block_num: BlockNumber) -> i64 {
    block_num.as_u32() as i64
}

use diesel::{Connection, RunQueryDsl, SqliteConnection};
use miden_objects::note::Nullifier;

use crate::errors::DatabaseError;

/// Returns the high 16 bits of the provided nullifier.
pub fn get_nullifier_prefix(nullifier: &Nullifier) -> u16 {
    (nullifier.most_significant_felt().as_int() >> 48) as u16
}

/// Checks if a table exists in the database.
#[allow(dead_code)]
pub fn table_exists(conn: &mut SqliteConnection, table_name: &str) -> Result<bool, DatabaseError> {
    conn.transaction(|conn| {
        let count =
            diesel::sql_query("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = $1")
                .bind::<diesel::sql_types::Text, &str>(table_name)
                .execute(conn)?;
        Ok::<bool, DatabaseError>(count > 0)
    })
}

/// Converts a slice of length `N` to an array, returns `None` if invariant
/// isn'crates/store/src/db/mod.rs upheld.
#[allow(dead_code)]
pub fn slice_to_array<const N: usize>(bytes: &[u8]) -> Option<[u8; N]> {
    if bytes.len() != N {
        return None;
    }
    let mut arr = [0u8; N];
    arr.copy_from_slice(bytes);
    Some(arr)
}

#[allow(dead_code)]
#[inline]
pub fn from_be_to_u32(bytes: &[u8]) -> Option<u32> {
    slice_to_array::<4>(bytes).map(u32::from_be_bytes)
}

#[derive(diesel::QueryableByName, Debug)]
#[diesel(table_name = diesel::table)]
pub struct PragmaSchemaVersion {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    pub schema_version: i32,
}

/// Returns the schema version of the database.
#[allow(dead_code)]
pub fn schema_version(conn: &mut SqliteConnection) -> Result<u32, DatabaseError> {
    let schema_version = conn.transaction(|conn| {
        let res = diesel::sql_query("SELECT schema_version FROM pragma_schema_version")
            .get_result::<PragmaSchemaVersion>(conn)?;
        Ok::<_, DatabaseError>(res.schema_version as u32)
    })?;
    Ok(schema_version)
}
