use diesel::prelude::Queryable;
use diesel::query_dsl::methods::SelectDsl;
use diesel::{
    ExpressionMethods,
    QueryDsl,
    QueryableByName,
    RunQueryDsl,
    Selectable,
    SqliteConnection,
};
use miden_lib::utils::Deserializable;
use miden_node_utils::limiter::{QueryParamAccountIdLimit, QueryParamLimiter};
use miden_objects::account::AccountId;
use miden_objects::block::BlockNumber;
use miden_objects::transaction::TransactionId;

use super::DatabaseError;
use crate::db::models::conv::SqlTypeConvert;
use crate::db::models::{serialize_vec, vec_raw_try_into};
use crate::db::{TransactionSummary, schema};

pub fn select_transactions_by_accounts_and_block_range(
    conn: &mut SqliteConnection,
    account_ids: &[AccountId],
    block_start: i64,
    block_end: i64, // TODO migrate to BlockNumber as argument type
) -> Result<Vec<TransactionSummary>, DatabaseError> {
    QueryParamAccountIdLimit::check(account_ids.len())?;

    // SELECT
    //     account_id,
    //     block_num,
    //     transaction_id
    // FROM
    //     transactions
    // WHERE
    //     block_num > ?1 AND
    //     block_num <= ?2 AND
    //     account_id IN rarray(?3)
    // ORDER BY
    //     transaction_id ASC
    let desired_account_ids = serialize_vec(account_ids);
    let raw = SelectDsl::select(
        schema::transactions::table,
        (
            schema::transactions::account_id,
            schema::transactions::block_num,
            schema::transactions::transaction_id,
        ),
    )
    .filter(schema::transactions::block_num.gt(block_start))
    .filter(schema::transactions::block_num.le(block_end))
    .filter(schema::transactions::account_id.eq_any(desired_account_ids))
    .order(schema::transactions::transaction_id.asc())
    .load::<TransactionSummaryRaw>(conn)
    .map_err(DatabaseError::from)?;
    Ok(vec_raw_try_into(raw).unwrap())
}

#[derive(Debug, Clone, PartialEq, Queryable, Selectable, QueryableByName)]
#[diesel(table_name = schema::transactions)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct TransactionSummaryRaw {
    account_id: Vec<u8>,
    block_num: i64,
    transaction_id: Vec<u8>,
}

impl TryInto<crate::db::TransactionSummary> for TransactionSummaryRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<crate::db::TransactionSummary, Self::Error> {
        Ok(crate::db::TransactionSummary {
            account_id: AccountId::read_from_bytes(&self.account_id[..])?,
            block_num: BlockNumber::from_raw_sql(self.block_num)?,
            transaction_id: TransactionId::read_from_bytes(&self.transaction_id[..])?,
        })
    }
}
