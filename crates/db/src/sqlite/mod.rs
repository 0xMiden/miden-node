//! A thin, additive SQLite framework over raw `rusqlite`.

mod codec;
mod in_list;
mod pool;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
mod tx;

pub use codec::{DbValue, DbValueRef, FromSqlValue, ToSqlValue};
pub use in_list::InList;
pub use pool::{DbReader, DbWriter, ReadTransaction, WriteTransaction, open, open_with_pool_size};
pub use tx::{ReadTx, Row, WriteTx};
