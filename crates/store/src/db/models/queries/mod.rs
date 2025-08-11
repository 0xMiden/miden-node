#![allow(
    clippy::needless_pass_by_value,
    reason = "The parent scope does own it, passing by value avoids additional boilerplate"
)]

use diesel::prelude::Queryable;
use diesel::query_dsl::methods::SelectDsl;
use diesel::{OptionalExtension, SqliteConnection, alias};
use miden_lib::utils::Serializable;
use miden_node_utils::limiter::{QueryParamLimiter, QueryParamNullifierLimit};
use miden_objects::account::AccountId;
use miden_objects::block::{BlockAccountUpdate, BlockHeader, BlockNumber};
use miden_objects::note::Nullifier;
use miden_objects::transaction::OrderedTransactionHeaders;

use super::{
    BoolExpressionMethods, DatabaseError, NoteSyncRecordRawRow, QueryDsl, RunQueryDsl,
    SelectableHelper,
};
use crate::db::models::conv::{SqlTypeConvert, nullifier_prefix_to_raw_sql, raw_sql_to_nonce};
use crate::db::models::{BigIntSum, ExpressionMethods, get_nullifier_prefix, sql_sum_into};
use crate::db::{NoteRecord, StateSyncUpdate, schema};
use crate::errors::StateSyncError;

mod transactions;
pub use transactions::*;
mod block_headers;
pub use block_headers::*;
mod notes;
pub(crate) use notes::*;
mod accounts;
pub use accounts::*;
mod nullifiers;
pub use nullifiers::*;

mod insertions;
pub(crate) use insertions::*;

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
