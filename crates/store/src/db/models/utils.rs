use miden_lib::utils::{Deserializable, DeserializationError, Serializable};
use miden_objects::block::BlockNumber;

use super::*;

/// Utility to convert an iteratable container of containing `R`-typed values
/// to a `Vec<D>` and bail at the first failing conversion
pub(crate) fn vec_raw_try_into<D, R: TryInto<D>>(
    raw: impl IntoIterator<Item = R>,
) -> std::result::Result<Vec<D>, <R as TryInto<D>>::Error> {
    std::result::Result::<Vec<D>, <R as TryInto<D>>::Error>::from_iter(
        raw.into_iter().map(<R as std::convert::TryInto<D>>::try_into),
    )
}

#[allow(dead_code)]
/// Deserialze an iteratable container full of byte blobs `B` to types `T`
pub(crate) fn deserialize_raw_vec<B: AsRef<[u8]>, T: Deserializable>(
    raw: impl IntoIterator<Item = B>,
) -> std::result::Result<Vec<T>, DeserializationError> {
    std::result::Result::<Vec<_>, DeserializationError>::from_iter(
        raw.into_iter().map(|raw| T::read_from_bytes(raw.as_ref())),
    )
}

/// Utility to convert an iteratable container to a vector of byte blobs
pub(crate) fn serialize_vec<'a, D: Serializable + 'a>(
    raw: impl IntoIterator<Item = &'a D>,
) -> Vec<Vec<u8>> {
    Vec::<_>::from_iter(raw.into_iter().map(<D as Serializable>::to_bytes))
}

/// Convert the database type `BigInt` into a blocknumber
///
/// Attention: We use `u32` as actual in-memory representation, since this will
/// suffice well beyond all our lifetimes.
pub(crate) fn raw_sql_to_block_number(raw: impl Into<i64>) -> BlockNumber {
    let raw = raw.into();
    #[allow(clippy::cast_sign_loss)]
    BlockNumber::from(raw as u32)
}
