#![allow(
    clippy::needless_pass_by_value,
    reason = "The parent scope does own it, passing by value avoids additional boilerplate"
)]

use diesel::prelude::Queryable;
use diesel::query_dsl::methods::SelectDsl;
use diesel::{OptionalExtension, SqliteConnection, alias};
use miden_lib::utils::Serializable;
use miden_node_utils::limiter::{
    QueryParamLimiter,
    QueryParamNullifierLimit,
    QueryParamNullifierPrefixLimit,
};
use miden_objects::account::AccountId;
use miden_objects::block::{BlockAccountUpdate, BlockHeader, BlockNumber};
use miden_objects::note::Nullifier;
use miden_objects::transaction::OrderedTransactionHeaders;

use super::super::models;
use super::{
    BoolExpressionMethods,
    DatabaseError,
    NoteSyncRecordRawRow,
    QueryDsl,
    RunQueryDsl,
    SelectableHelper,
};
use crate::db::models::conv::{SqlTypeConvert, nullifier_prefix_to_raw_sql, raw_sql_to_nonce};
use crate::db::models::{
    BigIntSum,
    ExpressionMethods,
    get_nullifier_prefix,
    sql_sum_into,
    vec_raw_try_into,
};
use crate::db::{NoteRecord, NullifierInfo, StateSyncUpdate, schema};
use crate::errors::StateSyncError;

mod transactions;
pub use transactions::*;
mod block_headers;
pub use block_headers::*;
mod notes;
pub(crate) use notes::*;
mod accounts;
pub use accounts::*;
mod insertions;
pub(crate) use insertions::*;

/// Select all nullifiers from the DB
///
/// # Returns
///
/// A vector with nullifiers and the block height at which they were created, or an error.
pub(crate) fn select_all_nullifiers(
    conn: &mut SqliteConnection,
) -> Result<Vec<NullifierInfo>, DatabaseError> {
    // SELECT nullifier, block_num FROM nullifiers ORDER BY block_num ASC
    let nullifiers_raw = SelectDsl::select(
        schema::nullifiers::table,
        models::NullifierWithoutPrefixRawRow::as_select(),
    )
    .load::<models::NullifierWithoutPrefixRawRow>(conn)?;
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

pub(crate) fn apply_block(
    conn: &mut SqliteConnection,
    block_header: &BlockHeader,
    notes: &[(NoteRecord, Option<Nullifier>)],
    nullifiers: &[Nullifier],
    accounts: &[BlockAccountUpdate],
    transactions: &OrderedTransactionHeaders,
) -> Result<usize, DatabaseError> {
    let mut count = 0;
    // Note: ordering here is important as the relevant tables have FK dependencies.
    count += insert_block_header(conn, block_header)?;
    count += upsert_accounts(conn, accounts, block_header.block_num())?;
    count += insert_scripts(conn, notes.iter().map(|(note, _)| note))?;
    count += insert_notes(conn, notes)?;
    count += insert_transactions(conn, block_header.block_num(), transactions)?;
    count += insert_nullifiers_for_block(conn, nullifiers, block_header.block_num())?;
    Ok(count)
}

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
    let nullifiers_raw = SelectDsl::select(
        schema::nullifiers::table,
        models::NullifierWithoutPrefixRawRow::as_select(),
    )
    .filter(schema::nullifiers::nullifier_prefix.eq_any(prefixes))
    .filter(schema::nullifiers::block_num.ge(block_num.to_raw_sql()))
    .order(schema::nullifiers::block_num.asc())
    .load::<models::NullifierWithoutPrefixRawRow>(conn)?;
    vec_raw_try_into(nullifiers_raw)
}

/// Select all accounts from the DB using the given [`SqliteConnection`].
///
/// # Returns
///
/// A vector with accounts, or an error.
#[cfg(test)]
pub(crate) fn select_all_accounts(
    conn: &mut SqliteConnection,
) -> Result<Vec<AccountInfo>, DatabaseError> {
    // SELECT
    //     account_id,
    //     account_commitment,
    //     block_num,
    //     details
    // FROM
    //     accounts
    // ORDER BY
    //     block_num ASC;

    let accounts_raw = QueryDsl::select(
        schema::accounts::table.left_join(schema::account_codes::table.on(
            schema::accounts::code_commitment.eq(schema::account_codes::code_commitment.nullable()),
        )),
        (models::AccountRaw::as_select(), schema::account_codes::code.nullable()),
    )
    .load::<(AccountRaw, Option<Vec<u8>>)>(conn)?;
    let account_infos = vec_raw_try_into::<AccountInfo, AccountWithCodeRaw>(
        accounts_raw.into_iter().map(AccountWithCodeRaw::from),
    )?;
    Ok(account_infos)
}

pub(crate) fn select_nonce_stmt(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    start_block_num: BlockNumber,
    end_block_num: BlockNumber,
) -> Result<Option<u64>, DatabaseError> {
    // SELECT
    //     SUM(nonce)
    // FROM
    //     account_deltas
    // WHERE
    //     account_id = ?1 AND block_num > ?2 AND block_num <= ?3
    let desired_account_id = account_id.to_bytes();
    let start_block_num = start_block_num.to_raw_sql();
    let end_block_num = end_block_num.to_raw_sql();

    // Note: The following does not work as anticipated for `BigInt` columns,
    // since `Foldable` for `BigInt` requires `Numeric`, which is only supported
    // with `numeric` and using `bigdecimal::BigDecimal`. It's a quite painful
    // to figure that out from the scattered comments and bits from the documentation.
    // Related <https://github.com/diesel-rs/diesel/discussions/4000>
    let maybe_nonce = SelectDsl::select(
        schema::account_deltas::table,
        // add type annotation for better error messages next time around someone tries to move to
        // `BigInt`
        diesel::dsl::sum::<diesel::sql_types::BigInt, _>(schema::account_deltas::nonce),
    )
    .filter(
        schema::account_deltas::account_id
            .eq(desired_account_id)
            .and(schema::account_deltas::block_num.gt(start_block_num))
            .and(schema::account_deltas::block_num.le(end_block_num)),
    )
    .get_result::<Option<BigIntSum>>(conn)
    .optional()?
    .flatten();

    maybe_nonce
        .map(|nonce_delta_sum| {
            let nonce = sql_sum_into::<i64, _>(&nonce_delta_sum, "account_deltas", "nonce")?;
            Ok::<_, DatabaseError>(raw_sql_to_nonce(nonce))
        })
        .transpose()
}

// Attention: A more complex query, utilizing aliases for nested queries
pub(crate) fn select_slot_updates_stmt(
    conn: &mut SqliteConnection,
    account_id_val: AccountId,
    start_block_num: BlockNumber,
    end_block_num: BlockNumber,
) -> Result<Vec<(i32, Vec<u8>)>, DatabaseError> {
    use schema::account_storage_slot_updates::dsl::{account_id, block_num, slot, value};

    // SELECT
    //     slot, value
    // FROM
    //     account_storage_slot_updates AS a
    // WHERE
    //     account_id = ?1 AND
    //     block_num > ?2 AND
    //     block_num <= ?3 AND
    //     NOT EXISTS(
    //         SELECT 1
    //         FROM account_storage_slot_updates AS b
    //         WHERE
    //             b.account_id = ?1 AND
    //             a.slot = b.slot AND
    //             a.block_num < b.block_num AND
    //             b.block_num <= ?3
    //     )
    let desired_account_id = account_id_val.to_bytes();
    let start_block_num = start_block_num.to_raw_sql();
    let end_block_num = end_block_num.to_raw_sql();

    // Alias the table for the inner and outer query
    let (a, b) = alias!(
        schema::account_storage_slot_updates as a,
        schema::account_storage_slot_updates as b
    );

    // Construct the NOT EXISTS subquery
    let subquery = b
        .filter(b.field(account_id).eq(&desired_account_id))
        .filter(a.field(slot).eq(b.field(slot))) // Correlated subquery: a.slot = b.slot
        .filter(a.field(block_num).lt(b.field(block_num))) // a.block_num < b.block_num
        .filter(b.field(block_num).le(end_block_num));

    // Construct the main query
    let results: Vec<(i32, Vec<u8>)> = SelectDsl::select(a, (a.field(slot), a.field(value)))
        .filter(a.field(account_id).eq(&desired_account_id))
        .filter(a.field(block_num).gt(start_block_num))
        .filter(a.field(block_num).le(end_block_num))
        .filter(diesel::dsl::not(diesel::dsl::exists(subquery))) // Apply the NOT EXISTS condition
        .load(conn)?;
    Ok(results)
}

#[derive(Debug, PartialEq, Eq, Clone, Queryable)]
pub(crate) struct StorageMapUpdateEntry {
    pub(crate) slot: i32,
    pub(crate) key: Vec<u8>,
    pub(crate) value: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq, Clone, Queryable)]
pub(crate) struct FungibleAssetDeltaEntry {
    pub(crate) faucet_id: Vec<u8>,
    pub(crate) value: i64,
}

/// Obtain a list of fungible asset delta statements
pub(crate) fn select_fungible_asset_deltas_stmt(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    start_block_num: BlockNumber,
    end_block_num: BlockNumber,
) -> Result<Vec<FungibleAssetDeltaEntry>, DatabaseError> {
    // SELECT
    //     faucet_id, SUM(delta)
    // FROM
    //     account_fungible_asset_deltas
    // WHERE
    //     account_id = ?1 AND
    //     block_num > ?2 AND
    //     block_num <= ?3
    // GROUP BY
    //     faucet_id
    let desired_account_id = account_id.to_bytes();
    let start_block_num = start_block_num.to_raw_sql();
    let end_block_num = end_block_num.to_raw_sql();

    let values: Vec<(Vec<u8>, Option<BigIntSum>)> = SelectDsl::select(
        schema::account_fungible_asset_deltas::table
            .filter(
                schema::account_fungible_asset_deltas::account_id
                    .eq(desired_account_id)
                    .and(schema::account_fungible_asset_deltas::block_num.gt(start_block_num))
                    .and(schema::account_fungible_asset_deltas::block_num.le(end_block_num)),
            )
            .group_by(schema::account_fungible_asset_deltas::faucet_id),
        (
            schema::account_fungible_asset_deltas::faucet_id,
            diesel::dsl::sum::<diesel::sql_types::BigInt, _>(
                schema::account_fungible_asset_deltas::delta,
            ),
        ),
    )
    .load(conn)?;
    let values = Result::<Vec<_>, _>::from_iter(values.into_iter().map(|(faucet_id, value)| {
        let value = value
            .map(|value| sql_sum_into::<i64, _>(&value, "fungible_asset_deltas", "delta"))
            .transpose()?
            .unwrap_or_default();
        Ok::<_, DatabaseError>(FungibleAssetDeltaEntry { faucet_id, value })
    }))?;
    Ok(values)
}

#[derive(Debug, PartialEq, Eq, Clone, Queryable)]
pub(crate) struct NonFungibleAssetDeltaEntry {
    pub(crate) block_num: i64,
    pub(crate) vault_key: Vec<u8>,
    pub(crate) is_remove: bool,
}

pub(crate) fn select_non_fungible_asset_updates_stmt(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    start_block_num: BlockNumber,
    end_block_num: BlockNumber,
) -> Result<Vec<NonFungibleAssetDeltaEntry>, DatabaseError> {
    // SELECT
    //     block_num, vault_key, is_remove
    // FROM
    //     account_non_fungible_asset_updates
    // WHERE
    //     account_id = ?1 AND
    //     block_num > ?2 AND
    //     block_num <= ?3
    // ORDER BY
    //     block_num
    let desired_account_id = account_id.to_bytes();
    let start_block_num = start_block_num.to_raw_sql();
    let end_block_num = end_block_num.to_raw_sql();

    let entries = SelectDsl::select(
        schema::account_non_fungible_asset_updates::table,
        (
            schema::account_non_fungible_asset_updates::block_num,
            schema::account_non_fungible_asset_updates::vault_key,
            schema::account_non_fungible_asset_updates::is_remove,
        ),
    )
    .filter(
        schema::account_non_fungible_asset_updates::account_id
            .eq(desired_account_id)
            .and(schema::account_non_fungible_asset_updates::block_num.gt(start_block_num))
            .and(schema::account_non_fungible_asset_updates::block_num.le(end_block_num)),
    )
    .order(schema::account_non_fungible_asset_updates::block_num.asc())
    .get_results(conn)?;
    Ok(entries)
}

/// Loads the state necessary for a state sync.
pub(crate) fn get_state_sync(
    conn: &mut SqliteConnection,
    since_block_number: BlockNumber,
    account_ids: Vec<AccountId>,
    note_tags: Vec<u32>,
) -> Result<StateSyncUpdate, StateSyncError> {
    // select notes since block by tag and sender
    let notes = select_notes_since_block_by_tag_and_sender(
        conn,
        since_block_number,
        &account_ids[..],
        &note_tags[..],
    )?;

    // select block header by block num
    let maybe_note_block_num = notes.first().map(|note| note.block_num);
    let block_header: BlockHeader = select_block_header_by_block_num(conn, maybe_note_block_num)?
        .ok_or_else(|| StateSyncError::EmptyBlockHeadersTable)?;

    // select accounts by block range
    let block_start = since_block_number.to_raw_sql();
    let block_end = block_header.block_num().to_raw_sql();
    let account_updates =
        select_accounts_by_block_range(conn, &account_ids, block_start, block_end)?;

    // select transactions by accounts and block range
    let transactions = select_transactions_by_accounts_and_block_range(
        conn,
        &account_ids,
        block_start,
        block_end,
    )?;
    Ok(StateSyncUpdate {
        notes,
        block_header,
        account_updates,
        transactions,
    })
}
