use core::any::type_name;

use miden_node_store::DatabaseTypeConversionError;
use miden_objects::block::BlockNumber;

/// Convert from and to it's database representation and back
///
/// We do not assume sanity of DB types.
pub trait SqlTypeConvert: Sized {
    type Raw: Sized;
    type Error: std::error::Error + Send + Sync + 'static;
    fn to_raw_sql(self) -> Self::Raw;
    #[allow(dead_code)]
    fn from_raw_sql(_raw: Self::Raw) -> Result<Self, Self::Error>;
}

impl SqlTypeConvert for BlockNumber {
    type Raw = i64;
    type Error = DatabaseTypeConversionError;
    fn from_raw_sql(raw: Self::Raw) -> Result<Self, Self::Error> {
        u32::try_from(raw)
            .map(BlockNumber::from)
            .map_err(|_| DatabaseTypeConversionError(type_name::<BlockNumber>()))
    }
    fn to_raw_sql(self) -> Self::Raw {
        i64::from(self.as_u32())
    }
}
