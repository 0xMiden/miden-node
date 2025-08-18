use std::collections::BTreeMap;

use diesel::prelude::{Queryable, QueryableByName};
use diesel::query_dsl::methods::SelectDsl;
use diesel::sqlite::Sqlite;
use diesel::{
    BoolExpressionMethods,
    ExpressionMethods,
    JoinOnDsl,
    NullableExpressionMethods,
    OptionalExtension,
    QueryDsl,
    RunQueryDsl,
    Selectable,
    SelectableHelper,
    SqliteConnection,
    alias,
};
use miden_lib::utils::{Deserializable, Serializable};
use miden_node_proto as proto;
use miden_node_proto::domain::account::{AccountInfo, AccountSummary};
use miden_node_utils::limiter::{QueryParamAccountIdLimit, QueryParamLimiter};
use miden_objects::account::{
    Account,
    AccountCode,
    AccountDelta,
    AccountId,
    AccountStorage,
    AccountStorageDelta,
    AccountVaultDelta,
    FungibleAssetDelta,
    NonFungibleAssetDelta,
    StorageMapDelta,
};
use miden_objects::asset::{AssetVault, NonFungibleAsset};
use miden_objects::block::BlockNumber;
use miden_objects::{Felt, Word};

use crate::db::models::conv::{SqlTypeConvert, raw_sql_to_nonce, raw_sql_to_slot};
use crate::db::models::{serialize_vec, vec_raw_try_into};
use crate::db::schema;
use crate::errors::DatabaseError;

/// Select the latest account details by account id from the DB using the given
/// [`SqliteConnection`].
///
/// # Returns
///
/// The latest account details, or an error.
pub(crate) fn select_account(
    conn: &mut SqliteConnection,
    account_id: AccountId,
) -> Result<proto::domain::account::AccountInfo, DatabaseError> {
    // SELECT
    //     account_id,
    //     account_commitment,
    //     block_num,
    //     details
    // FROM
    //     accounts
    // WHERE
    //     account_id = ?1;
    //

    let raw = SelectDsl::select(
        schema::accounts::table.left_join(schema::account_codes::table.on(
            schema::accounts::code_commitment.eq(schema::account_codes::code_commitment.nullable()),
        )),
        (AccountRaw::as_select(), schema::account_codes::code.nullable()),
    )
    .filter(schema::accounts::account_id.eq(account_id.to_bytes()))
    .get_result::<(AccountRaw, Option<Vec<u8>>)>(conn)
    .optional()?
    .ok_or(DatabaseError::AccountNotFoundInDb(account_id))?;
    let info = AccountWithCodeRaw::from(raw).try_into()?;
    Ok(info)
}

// TODO: Handle account prefix collision in a more robust way
/// Select the latest account details by account ID prefix from the DB using the given
/// [`SqliteConnection`] This method is meant to be used by the network transaction builder. Because
/// network notes get matched through accounts through the account's 30-bit prefix, it is possible
/// that multiple accounts match against a single prefix. In this scenario, the first account is
/// returned.
///
/// # Returns
///
/// The latest account details, `None` if the account was not found, or an error.
pub(crate) fn select_account_by_id_prefix(
    conn: &mut SqliteConnection,
    id_prefix: u32,
) -> Result<Option<AccountInfo>, DatabaseError> {
    // SELECT
    //     account_id,
    //     account_commitment,
    //     block_num,
    //     details
    // FROM
    //     accounts
    // WHERE
    //     network_account_id_prefix = ?1;
    let maybe_info = SelectDsl::select(
        schema::accounts::table.left_join(schema::account_codes::table.on(
            schema::accounts::code_commitment.eq(schema::account_codes::code_commitment.nullable()),
        )),
        (AccountRaw::as_select(), schema::account_codes::code.nullable()),
    )
    .filter(schema::accounts::network_account_id_prefix.eq(Some(i64::from(id_prefix))))
    .get_result::<(AccountRaw, Option<Vec<u8>>)>(conn)
    .optional()
    .map_err(DatabaseError::Diesel)?;

    let result: Result<Option<AccountInfo>, DatabaseError> = maybe_info
        .map(AccountWithCodeRaw::from)
        .map(std::convert::TryInto::<AccountInfo>::try_into)
        .transpose();

    result
}

/// Select all account commitments from the DB using the given [`SqliteConnection`].
///
/// # Returns
///
/// The vector with the account id and corresponding commitment, or an error.
pub(crate) fn select_all_account_commitments(
    conn: &mut SqliteConnection,
) -> Result<Vec<(AccountId, Word)>, DatabaseError> {
    // SELECT account_id, account_commitment FROM accounts ORDER BY block_num ASC
    let raw = SelectDsl::select(
        schema::accounts::table,
        (schema::accounts::account_id, schema::accounts::account_commitment),
    )
    .order_by(schema::accounts::block_num.asc())
    .load::<(Vec<u8>, Vec<u8>)>(conn)?;

    Result::<Vec<_>, DatabaseError>::from_iter(raw.into_iter().map(
        |(ref account, ref commitment)| {
            Ok((AccountId::read_from_bytes(account)?, Word::read_from_bytes(commitment)?))
        },
    ))
}

pub(crate) fn select_accounts_by_id(
    conn: &mut SqliteConnection,
    account_ids: Vec<AccountId>,
) -> Result<Vec<AccountInfo>, DatabaseError> {
    QueryParamAccountIdLimit::check(account_ids.len())?;

    let account_ids = account_ids.iter().map(|account_id| account_id.to_bytes().clone());

    let accounts_raw = SelectDsl::select(
        schema::accounts::table.left_join(schema::account_codes::table.on(
            schema::accounts::code_commitment.eq(schema::account_codes::code_commitment.nullable()),
        )),
        (AccountRaw::as_select(), schema::account_codes::code.nullable()),
    )
    .filter(schema::accounts::account_id.eq_any(account_ids))
    .load::<(AccountRaw, Option<Vec<u8>>)>(conn)?;
    let account_infos = vec_raw_try_into::<AccountInfo, AccountWithCodeRaw>(
        accounts_raw.into_iter().map(AccountWithCodeRaw::from),
    )?;
    Ok(account_infos)
}

/// Selects and merges account deltas by account id and block range from the DB using the given
/// [`SqliteConnection`].
///
/// # Note:
///
/// `block_start` is exclusive and `block_end` is inclusive.
///
/// # Returns
///
/// The resulting account delta, or an error.
#[allow(
    clippy::cast_sign_loss,
    reason = "Slot types have a DB representation of i32, in memory they are u8"
)]
pub(crate) fn select_account_delta(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    block_start: BlockNumber,
    block_end: BlockNumber,
) -> Result<Option<AccountDelta>, DatabaseError> {
    let slot_updates = select_slot_updates_stmt(conn, account_id, block_start, block_end)?;
    let storage_map_updates =
        select_storage_map_updates_stmt(conn, account_id, block_start, block_end)?;
    let fungible_asset_deltas =
        select_fungible_asset_deltas_stmt(conn, account_id, block_start, block_end)?;
    let non_fungible_asset_updates =
        select_non_fungible_asset_updates_stmt(conn, account_id, block_start, block_end)?;

    let Some(nonce) = select_nonce_stmt(conn, account_id, block_start, block_end)? else {
        return Ok(None);
    };
    let nonce = nonce.try_into().map_err(DatabaseError::InvalidFelt)?;

    let storage_scalars =
        Result::<BTreeMap<u8, Word>, _>::from_iter(slot_updates.iter().map(|(slot, value)| {
            let felt = Word::read_from_bytes(value)?;
            Ok::<_, DatabaseError>((raw_sql_to_slot(*slot), felt))
        }))?;

    let mut storage_maps = BTreeMap::<u8, StorageMapDelta>::new();
    for StorageMapUpdateEntry { slot, key, value } in storage_map_updates {
        let key = Word::read_from_bytes(&key)?;
        let value = Word::read_from_bytes(&value)?;
        storage_maps.entry(slot as u8).or_default().insert(key, value);
    }

    let mut fungible = BTreeMap::<AccountId, i64>::new();
    for FungibleAssetDeltaEntry { faucet_id, value } in fungible_asset_deltas {
        let faucet_id = AccountId::read_from_bytes(&faucet_id)?;
        fungible.insert(faucet_id, value);
    }

    let mut non_fungible_delta = NonFungibleAssetDelta::default();
    for NonFungibleAssetDeltaEntry {
        vault_key: vault_key_asset, is_remove, ..
    } in non_fungible_asset_updates
    {
        let asset = NonFungibleAsset::read_from_bytes(&vault_key_asset)
            .map_err(|err| DatabaseError::DataCorrupted(err.to_string()))?;

        if is_remove {
            non_fungible_delta.remove(asset)?;
        } else {
            non_fungible_delta.add(asset)?;
        }
    }

    let storage = AccountStorageDelta::from_parts(storage_scalars, storage_maps)?;
    let vault = AccountVaultDelta::new(FungibleAssetDelta::new(fungible)?, non_fungible_delta);

    Ok(Some(AccountDelta::new(account_id, storage, vault, nonce)?))
}

/// Select [`AccountSummary`] from the DB using the given [`SqliteConnection`], given that the
/// account update was done between `(block_start, block_end]`.
///
/// # Returns
///
/// The vector of [`AccountSummary`] with the matching accounts.
pub fn select_accounts_by_block_range(
    conn: &mut SqliteConnection,
    account_ids: &[AccountId],
    block_start: i64,
    block_end: i64,
) -> Result<Vec<AccountSummary>, DatabaseError> {
    QueryParamAccountIdLimit::check(account_ids.len())?;

    // SELECT
    //     account_id,
    //     account_commitment,
    //     block_num
    // FROM
    //     accounts
    // WHERE
    //     block_num > ?1 AND
    //     block_num <= ?2 AND
    //     account_id IN rarray(?3)
    // ORDER BY
    //     block_num ASC
    let desired_account_ids = serialize_vec(account_ids);
    let raw: Vec<AccountSummaryRaw> =
        SelectDsl::select(schema::accounts::table, AccountSummaryRaw::as_select())
            .filter(schema::accounts::block_num.gt(block_start))
            .filter(schema::accounts::block_num.le(block_end))
            .filter(schema::accounts::account_id.eq_any(desired_account_ids))
            .order(schema::accounts::block_num.asc())
            .load::<AccountSummaryRaw>(conn)?;
    // SAFETY `From` implies `TryFrom<Error=Infallible`, which is the case for `AccountSummaryRaw`
    // -> `AccountSummary`
    Ok(vec_raw_try_into(raw).unwrap())
}

#[allow(clippy::type_complexity)]
pub(crate) fn select_storage_map_updates_stmt(
    conn: &mut SqliteConnection,
    account_id_val: AccountId,
    start_block_num: BlockNumber,
    end_block_num: BlockNumber,
) -> Result<Vec<StorageMapUpdateEntry>, DatabaseError> {
    // SELECT
    //     slot, key, value
    // FROM
    //     account_storage_map_updates AS a
    // WHERE
    //     account_id = ?1 AND
    //     block_num > ?2 AND
    //     block_num <= ?3 AND
    //     NOT EXISTS(
    //         SELECT 1
    //         FROM account_storage_map_updates AS b
    //         WHERE
    //             b.account_id = ?1 AND
    //             a.slot = b.slot AND
    //             a.key = b.key AND
    //             a.block_num < b.block_num AND
    //             b.block_num <= ?3
    //     )
    use schema::account_storage_map_updates::dsl::{account_id, block_num, key, slot, value};

    let desired_account_id = account_id_val.to_bytes();
    let start_block_num = start_block_num.to_raw_sql();
    let end_block_num = end_block_num.to_raw_sql();

    // Alias the table for the inner and outer query
    let (a, b) = alias!(
        schema::account_storage_map_updates as a,
        schema::account_storage_map_updates as b
    );

    // Construct the NOT EXISTS subquery
    let subquery = b
        .filter(b.field(account_id).eq(&desired_account_id))
        .filter(a.field(slot).eq(b.field(slot))) // Correlated subquery: a.slot = b.slot
        .filter(a.field(block_num).lt(b.field(block_num))) // a.block_num < b.block_num
        .filter(b.field(block_num).le(end_block_num));

    // Construct the main query
    let res: Vec<StorageMapUpdateEntry> = SelectDsl::select(a, (a.field(slot), a.field(key), a.field(value)))
        .filter(a.field(account_id).eq(&desired_account_id))
        .filter(a.field(block_num).gt(start_block_num))
        .filter(a.field(block_num).le(end_block_num))
        .filter(diesel::dsl::not(diesel::dsl::exists(subquery))) // Apply the NOT EXISTS condition
        .load(conn)?;
    Ok(res)
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
        (AccountRaw::as_select(), schema::account_codes::code.nullable()),
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

/// Select account storage map values from the DB using the given [`SqliteConnection`].
///
/// # Returns
///
/// A vector of tuples containing `(slot, key, value, is_latest_update)` for the given account and
/// block range.
#[cfg(test)]
pub(crate) fn select_account_storage_map_values(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    block_num: Option<BlockNumber>,
) -> Result<Vec<(u8, Word, Word, bool)>, DatabaseError> {
    let base = SelectDsl::select(
        schema::account_storage_map_values::table,
        (
            schema::account_storage_map_values::slot,
            schema::account_storage_map_values::key,
            schema::account_storage_map_values::value,
            schema::account_storage_map_values::is_latest_update,
        ),
    )
    .filter(schema::account_storage_map_values::account_id.eq(account_id.to_bytes()));

    let mut query = base.into_boxed();

    if let Some(block_num) = block_num {
        query =
            query.filter(schema::account_storage_map_values::block_num.eq(block_num.to_raw_sql()));
    }

    let results: Vec<(i32, Vec<u8>, Vec<u8>, i32)> = query.load(conn)?;

    results
        .into_iter()
        .map(|(slot, key, value, is_latest_update)| -> Result<(u8, Word, Word, bool), DatabaseError> {
            let key = Word::read_from_bytes(&key)?;
            let value = Word::read_from_bytes(&value)?;
            Ok((raw_sql_to_slot(slot), key, value, is_latest_update != 0))
        })
        .collect()
}

/// Select the latest account storage map values (where `is_latest_update`=true) from the DB.
///
/// # Returns
///
/// A vector of tuples containing (slot, key, value) for the latest values only.
#[cfg(test)]
pub(crate) fn select_latest_account_storage_map_values(
    conn: &mut SqliteConnection,
    account_id: AccountId,
) -> Result<Vec<(u8, Word, Word)>, DatabaseError> {
    let results: Vec<(i32, Vec<u8>, Vec<u8>)> = SelectDsl::select(
        schema::account_storage_map_values::table.filter(
            schema::account_storage_map_values::account_id
                .eq(account_id.to_bytes())
                .and(schema::account_storage_map_values::is_latest_update.eq(1)),
        ),
        (
            schema::account_storage_map_values::slot,
            schema::account_storage_map_values::key,
            schema::account_storage_map_values::value,
        ),
    )
    .load(conn)?;

    results
        .into_iter()
        .map(|(slot, key, value)| -> Result<(u8, Word, Word), DatabaseError> {
            let key = Word::read_from_bytes(&key)?;
            let value = Word::read_from_bytes(&value)?;
            Ok((raw_sql_to_slot(slot), key, value))
        })
        .collect()
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

/// A type to represent a `sum(BigInt)`
// TODO: make this a type, but it's unclear how that should work
// See: <https://github.com/diesel-rs/diesel/discussions/4684>
pub type BigIntSum = bigdecimal::BigDecimal;

/// Impractical conversion required for `diesel-rs` `sum(BigInt)` results.
pub fn sql_sum_into<T, E>(
    sum: &BigIntSum,
    table: &'static str,
    column: &'static str,
) -> Result<T, DatabaseError>
where
    E: std::error::Error + Send + Sync + 'static,
    T: TryFrom<bigdecimal::num_bigint::BigInt, Error = E>,
{
    let (val, exponent) = sum.as_bigint_and_exponent();
    debug_assert_eq!(
        exponent, 0,
        "We only sum(integers), hence there must never be a decimal result"
    );
    let val = T::try_from(val).map_err(|e| DatabaseError::ColumnSumExceedsLimit {
        table,
        column,
        limit: "<T>::MAX",
        source: Box::new(e),
    })?;
    Ok::<_, DatabaseError>(val)
}

#[derive(Debug, Clone, Queryable, QueryableByName, Selectable)]
#[diesel(table_name = schema::accounts)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct AccountRaw {
    pub account_id: Vec<u8>,
    pub account_commitment: Vec<u8>,
    pub block_num: i64,
    pub storage: Option<Vec<u8>>,
    pub vault: Option<Vec<u8>>,
    pub nonce: Option<i64>,
}

#[derive(Debug, Clone, QueryableByName)]
pub struct AccountWithCodeRaw {
    #[diesel(embed)]
    pub account: AccountRaw,
    #[diesel(embed)]
    pub code: Option<Vec<u8>>,
}

impl From<(AccountRaw, Option<Vec<u8>>)> for AccountWithCodeRaw {
    fn from((account, code): (AccountRaw, Option<Vec<u8>>)) -> Self {
        Self { account, code }
    }
}

impl TryInto<proto::domain::account::AccountInfo> for AccountWithCodeRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<proto::domain::account::AccountInfo, Self::Error> {
        use proto::domain::account::{AccountInfo, AccountSummary};

        let account_id = AccountId::read_from_bytes(&self.account.account_id[..])?;
        let account_commitment = Word::read_from_bytes(&self.account.account_commitment[..])?;
        let block_num = BlockNumber::from_raw_sql(self.account.block_num)?;
        let summary = AccountSummary {
            account_id,
            account_commitment,
            block_num,
        };
        let maybe_account = self.try_into()?;
        Ok(AccountInfo { summary, details: maybe_account })
    }
}

impl TryInto<Option<Account>> for AccountWithCodeRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<Option<Account>, Self::Error> {
        let account_id = AccountId::read_from_bytes(&self.account.account_id[..])?;

        let details = if let (Some(vault), Some(storage), Some(nonce), Some(code)) =
            (self.account.vault, self.account.storage, self.account.nonce, self.code)
        {
            let vault = AssetVault::read_from_bytes(&vault)?;
            let storage = AccountStorage::read_from_bytes(&storage)?;
            let code = AccountCode::read_from_bytes(&code)?;
            let nonce = raw_sql_to_nonce(nonce);
            let nonce = Felt::new(nonce);
            Some(Account::from_parts(account_id, vault, storage, code, nonce))
        } else {
            // a private account
            None
        };
        Ok(details)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Selectable, Queryable, QueryableByName)]
#[diesel(table_name = schema::accounts)]
#[diesel(check_for_backend(Sqlite))]
pub struct AccountSummaryRaw {
    account_id: Vec<u8>,         // AccountId,
    account_commitment: Vec<u8>, //RpoDigest,
    block_num: i64,              //BlockNumber,
}

impl TryInto<AccountSummary> for AccountSummaryRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<AccountSummary, Self::Error> {
        let account_id = AccountId::read_from_bytes(&self.account_id[..])?;
        let account_commitment = Word::read_from_bytes(&self.account_commitment[..])?;
        let block_num = BlockNumber::from_raw_sql(self.block_num)?;

        Ok(AccountSummary {
            account_id,
            account_commitment,
            block_num,
        })
    }
}
