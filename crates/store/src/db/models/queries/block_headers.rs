use diesel::prelude::{AsChangeset, Insertable};
use diesel::{OptionalExtension, SelectableHelper, SqliteConnection};
use miden_node_utils::limiter::QueryParamBlockLimit;
use miden_objects::block::{BlockHeader, BlockNumber};

use super::{DatabaseError, QueryDsl, Queryable, QueryableByName, Selectable, Sqlite};
use crate::db::models::conv::{
    aux_to_raw_sql, execution_hint_to_raw_sql, execution_mode_to_raw_sql, idx_to_raw_sql,
    note_type_to_raw_sql, raw_sql_to_nonce,
};
use crate::db::models::{self, ExpressionMethods, vec_raw_try_into};
use crate::db::schema;

/// Select a [`BlockHeader`] from the DB by its `block_num` using the given [`SqliteConnection`].
///
/// # Returns
///
/// When `block_number` is [None], the latest block header is returned. Otherwise, the block with
/// the given block height is returned.
pub(crate) fn select_block_header_by_block_num(
    conn: &mut SqliteConnection,
    maybe_block_number: Option<BlockNumber>,
) -> Result<Option<BlockHeader>, DatabaseError> {
    let sel = SelectDsl::select(schema::block_headers::table, models::BlockHeaderRaw::as_select());
    let row = if let Some(block_number) = maybe_block_number {
        // SELECT block_header FROM block_headers WHERE block_num = ?1
        sel.filter(schema::block_headers::block_num.eq(block_number.to_raw_sql()))
            .get_result::<models::BlockHeaderRaw>(conn)
            .optional()?
        // invariant: only one block exists with the given block header, so the length is
        // always zero or one
    } else {
        // SELECT block_header FROM block_headers ORDER BY block_num DESC LIMIT 1
        sel.order(schema::block_headers::block_num.desc())
            .limit(1)
            .get_result::<models::BlockHeaderRaw>(conn)
            .optional()?
    };
    row.map(std::convert::TryInto::try_into).transpose()
}

/// Select all the given block headers from the DB using the given [`SqliteConnection`].
///
/// # Note
///
/// Only returns the block headers that are actually present.
///
/// # Returns
pub fn select_block_headers(
    conn: &mut SqliteConnection,
    blocks: impl Iterator<Item = BlockNumber> + Send,
) -> Result<Vec<BlockHeader>, DatabaseError> {
    // The iterators are all deterministic, so is the conjunction.
    // All calling sites do it equivalently, hence the below holds.
    // <https://doc.rust-lang.org/src/core/slice/iter/macros.rs.html#195>
    // <https://doc.rust-lang.org/src/core/option.rs.html#2273>
    // And the conjunction is truthful:
    // <https://doc.rust-lang.org/src/core/iter/adapters/chain.rs.html#184>
    QueryParamBlockLimit::check(blocks.size_hint().0)?;

    // SELECT block_header FROM block_headers WHERE block_num IN rarray(?1)
    let blocks = Vec::from_iter(blocks.map(SqlTypeConvert::to_raw_sql));
    let raw_block_headers =
        QueryDsl::select(schema::block_headers::table, models::BlockHeaderRaw::as_select())
            .filter(schema::block_headers::block_num.eq_any(blocks))
            .load::<models::BlockHeaderRaw>(conn)?;
    vec_raw_try_into(raw_block_headers)
}

/// Select all block headers from the DB using the given [`SqliteConnection`].
///
/// # Returns
///
/// A vector of [`BlockHeader`] or an error.
pub fn select_all_block_headers(
    conn: &mut SqliteConnection,
) -> Result<Vec<BlockHeader>, DatabaseError> {
    // SELECT block_header FROM block_headers ORDER BY block_num ASC
    let raw_block_headers =
        QueryDsl::select(schema::block_headers::table, models::BlockHeaderRaw::as_select())
            .order(schema::block_headers::block_num.asc())
            .load::<models::BlockHeaderRaw>(conn)?;
    vec_raw_try_into(raw_block_headers)
}

#[derive(Debug, Clone, Queryable, QueryableByName, Selectable)]
#[diesel(table_name = schema::block_headers)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct BlockHeaderRaw {
    #[allow(dead_code)]
    pub block_num: i64,
    pub block_header: Vec<u8>,
}
impl TryInto<BlockHeader> for BlockHeaderRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<BlockHeader, Self::Error> {
        let block_header = BlockHeader::read_from_bytes(&self.block_header[..])?;
        Ok(block_header)
    }
}
