use diesel::query_dsl::methods::SelectDsl;
use diesel::{
    ExpressionMethods,
    QueryDsl,
    Queryable,
    QueryableByName,
    RunQueryDsl,
    Selectable,
    SelectableHelper,
    SqliteConnection,
};
use miden_lib::utils::{Deserializable, Serializable};
use miden_node_utils::limiter::{
    QueryParamLimiter,
    QueryParamNullifierLimit,
    QueryParamNullifierPrefixLimit,
};
use miden_objects::block::BlockNumber;
use miden_objects::note::Nullifier;

use super::DatabaseError;
use crate::db::models::conv::{SqlTypeConvert, nullifier_prefix_to_raw_sql};
use crate::db::models::utils::{get_nullifier_prefix, vec_raw_try_into};
use crate::db::{NullifierInfo, schema};

/// Returns nullifiers filtered by prefix within a block number range.
///
/// Each value of the `nullifier_prefixes` is only the `prefix_len` most significant bits of the
/// nullifier of interest to the client. This hides the details of the specific nullifier being
/// requested. Currently the only supported prefix length is 16 bits.
///
/// # Returns
///
/// Range and pagination semantics:
/// - Both `block_from` and `block_to` are inclusive bounds.
/// - To keep responses ≤ ~2MB, an internal row limit is enforced. When the limit is hit and the
///   result spans multiple blocks, the last block in the page is dropped entirely, and
///  `last_block_included` is set to the block number immediately before that dropped block.
///   Clients should resume with `block_from = last_block_included + 1`.
/// - If all rows belong to a single block and hit the limit, this function returns an empty
///   `nullifiers` page with `last_block_included = that_block - 1`. Intra-block pagination is not
///   supported; callers may need to narrow the range.
///
/// A tuple `(nullifiers, last_block_included)` where:
/// - `nullifiers` is a vector of [`NullifierInfo`] (each contains the nullifier and the block
///   number at which it was created), ordered by block number ascending.
/// - `last_block_included` is the last block number fully included in this response. If the
///   internal row limit is reached (to cap response size), this may be less than `block_to`. In
///   that case, the caller should re-issue the query with `block_from = last_block_included + 1` to
///   continue.
pub(crate) fn select_nullifiers_by_prefix(
    conn: &mut SqliteConnection,
    prefix_len: u8,
    nullifier_prefixes: &[u16],
    block_from: BlockNumber,
    block_to: BlockNumber,
) -> Result<(Vec<NullifierInfo>, BlockNumber), DatabaseError> {
    pub const MAX_PAYLOAD_BYTES: usize = 2 * 1024 * 1024; // 2 MB
    pub const NULLIFIER_BYTES: usize = 32; // digest size
    pub const ROW_OVERHEAD_BYTES: usize = NULLIFIER_BYTES + size_of::<i64>(); // 40 bytes
    pub const ROW_LIMIT: usize = (MAX_PAYLOAD_BYTES / ROW_OVERHEAD_BYTES) + 1;

    assert_eq!(prefix_len, 16, "Only 16-bit prefixes are supported");

    if block_from > block_to {
        return Err(DatabaseError::InvalidBlockRange { from: block_from, to: block_to });
    }

    QueryParamNullifierPrefixLimit::check(nullifier_prefixes.len())?;

    // SELECT
    //     nullifier,
    //     block_num
    // FROM
    //     nullifiers
    // WHERE
    //     nullifier_prefix IN rarray(?1) AND
    //     block_num >= ?2
    //     AND block_num <= ?3
    // ORDER BY
    //     block_num ASC
    // LIMIT
    //     ROW_LIMIT;

    let prefixes = nullifier_prefixes.iter().map(|prefix| nullifier_prefix_to_raw_sql(*prefix));
    let raw =
        SelectDsl::select(schema::nullifiers::table, NullifierWithoutPrefixRawRow::as_select())
            .filter(schema::nullifiers::nullifier_prefix.eq_any(prefixes))
            .filter(schema::nullifiers::block_num.ge(block_from.to_raw_sql()))
            .filter(schema::nullifiers::block_num.le(block_to.to_raw_sql()))
            .order(schema::nullifiers::block_num.asc())
            .limit(i64::try_from(ROW_LIMIT).expect("limit fits within i64"))
            .load::<NullifierWithoutPrefixRawRow>(conn)?;

    // Discard the last block in the response (assumes more than one block may be present)
    if raw.len() >= ROW_LIMIT {
        // SAFETY: len >= ROW_LIMIT => non-empty
        let last_block_num_i64 = raw.last().unwrap().block_num;

        let nullifiers = vec_raw_try_into(
            raw.into_iter().take_while(|row| row.block_num != last_block_num_i64),
        )?;

        let last_block_included = BlockNumber::from_raw_sql(last_block_num_i64.saturating_sub(1))?;

        Ok((nullifiers, last_block_included))
    } else {
        Ok((vec_raw_try_into(raw)?, block_to))
    }
}

/// Select all nullifiers from the DB
///
/// # Returns
///
/// A vector with nullifiers and the block height at which they were created, or an error.
pub(crate) fn select_all_nullifiers(
    conn: &mut SqliteConnection,
) -> Result<Vec<NullifierInfo>, DatabaseError> {
    // SELECT nullifier, block_num FROM nullifiers ORDER BY block_num ASC
    let nullifiers_raw =
        SelectDsl::select(schema::nullifiers::table, NullifierWithoutPrefixRawRow::as_select())
            .load::<NullifierWithoutPrefixRawRow>(conn)?;
    vec_raw_try_into(nullifiers_raw)
}

/// Commit nullifiers to the DB using the given [`SqliteConnection`]. This inserts the nullifiers
/// into the nullifiers table, and marks the note as consumed (if it was public).
///
/// # Returns
///
/// The number of affected rows.
///
/// # Note
///
/// The [`SqliteConnection`] object is not consumed. It's up to the caller to commit or rollback the
/// transaction.
pub(crate) fn insert_nullifiers_for_block(
    conn: &mut SqliteConnection,
    nullifiers: &[Nullifier],
    block_num: BlockNumber,
) -> Result<usize, DatabaseError> {
    QueryParamNullifierLimit::check(nullifiers.len())?;

    // UPDATE notes SET consumed = TRUE WHERE nullifier IN rarray(?1)
    let serialized_nullifiers =
        Vec::<Vec<u8>>::from_iter(nullifiers.iter().map(Nullifier::to_bytes));

    let mut count = diesel::update(schema::notes::table)
        .filter(schema::notes::nullifier.eq_any(&serialized_nullifiers))
        .set(schema::notes::consumed_at.eq(Some(block_num.to_raw_sql())))
        .execute(conn)?;

    count += diesel::insert_into(schema::nullifiers::table)
        .values(Vec::from_iter(nullifiers.iter().zip(serialized_nullifiers.iter()).map(
            |(nullifier, bytes)| {
                (
                    schema::nullifiers::nullifier.eq(bytes),
                    schema::nullifiers::nullifier_prefix
                        .eq(nullifier_prefix_to_raw_sql(get_nullifier_prefix(nullifier))),
                    schema::nullifiers::block_num.eq(block_num.to_raw_sql()),
                )
            },
        )))
        .execute(conn)?;

    Ok(count)
}

#[derive(Debug, Clone, Queryable, QueryableByName, Selectable)]
#[diesel(table_name = schema::nullifiers)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct NullifierWithoutPrefixRawRow {
    pub nullifier: Vec<u8>,
    pub block_num: i64,
}

impl TryInto<NullifierInfo> for NullifierWithoutPrefixRawRow {
    type Error = DatabaseError;
    fn try_into(self) -> Result<NullifierInfo, Self::Error> {
        let nullifier = Nullifier::read_from_bytes(&self.nullifier)?;
        let block_num = BlockNumber::from_raw_sql(self.block_num)?;
        Ok(NullifierInfo { nullifier, block_num })
    }
}
