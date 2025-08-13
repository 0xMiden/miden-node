use bigdecimal::BigDecimal;
use diesel::query_dsl::methods::SelectDsl;
use diesel::{SelectableHelper, SqliteConnection};
use miden_lib::utils::Serializable;
use miden_node_proto::domain::account::AccountSummary;
use miden_node_proto::{self as proto};
use miden_node_utils::limiter::{
    QueryParamLimiter, QueryParamNullifierLimit, QueryParamNullifierPrefixLimit,
};
use miden_objects::account::{Account, AccountCode, AccountId, AccountStorage};
use miden_objects::asset::AssetVault;
use miden_objects::block::BlockNumber;
use miden_objects::crypto::merkle::SparseMerklePath;
use miden_objects::note::{
    NoteAssets, NoteDetails, NoteExecutionHint, NoteInputs, NoteMetadata, NoteRecipient,
    NoteScript, NoteTag, NoteType, Nullifier,
};
use miden_objects::transaction::TransactionId;
use miden_objects::{Felt, Word};

use super::{
    DatabaseError, NoteRecord, NullifierInfo, QueryDsl, Queryable, QueryableByName, RunQueryDsl,
    Selectable, Sqlite,
};
use crate::db::models::conv::{
    SqlTypeConvert, aux_to_raw_sql, execution_hint_to_raw_sql, execution_mode_to_raw_sql,
    idx_to_raw_sql, note_type_to_raw_sql, nullifier_prefix_to_raw_sql, raw_sql_to_nonce,
};
use crate::db::models::{ExpressionMethods, get_nullifier_prefix, vec_raw_try_into};
use crate::db::{NullifierInfo, schema};

/// Returns nullifiers filtered by prefix and block creation height.
///
/// Each value of the `nullifier_prefixes` is only the `prefix_len` most significant bits
/// of the nullifier of interest to the client. This hides the details of the specific
/// nullifier being requested. Currently the only supported prefix length is 16 bits.
///
/// # Returns
///
/// A vector of [`NullifierInfo`] with the nullifiers and the block height at which they were
pub(crate) fn select_nullifiers_by_prefix(
    conn: &mut SqliteConnection,
    prefix_len: u8,
    nullifier_prefixes: &[u16],
    block_num: BlockNumber,
) -> Result<Vec<NullifierInfo>, DatabaseError> {
    assert_eq!(prefix_len, 16, "Only 16-bit prefixes are supported");

    QueryParamNullifierPrefixLimit::check(nullifier_prefixes.len())?;

    // SELECT
    //     nullifier,
    //     block_num
    // FROM
    //     nullifiers
    // WHERE
    //     nullifier_prefix IN rarray(?1) AND
    //     block_num >= ?2
    // ORDER BY
    //     block_num ASC

    let prefixes = nullifier_prefixes.iter().map(|prefix| nullifier_prefix_to_raw_sql(*prefix));
    let nullifiers_raw =
        SelectDsl::select(schema::nullifiers::table, NullifierWithoutPrefixRawRow::as_select())
            .filter(schema::nullifiers::nullifier_prefix.eq_any(prefixes))
            .filter(schema::nullifiers::block_num.ge(block_num.to_raw_sql()))
            .order(schema::nullifiers::block_num.asc())
            .load::<NullifierWithoutPrefixRawRow>(conn)?;
    vec_raw_try_into(nullifiers_raw)
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
