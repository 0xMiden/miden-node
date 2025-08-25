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
};
use miden_lib::utils::{Deserializable, Serializable};
use miden_node_proto as proto;
use miden_node_proto::domain::account::{AccountInfo, AccountSummary};
use miden_node_utils::limiter::{QueryParamAccountIdLimit, QueryParamLimiter};
use miden_objects::account::{Account, AccountCode, AccountId, AccountStorage};
use miden_objects::asset::{Asset, AssetVault};
use miden_objects::block::BlockNumber;
use miden_objects::{Felt, Word};

use crate::db::models::conv::{SqlTypeConvert, raw_sql_to_nonce, raw_sql_to_slot};
use crate::db::models::{serialize_vec, vec_raw_try_into};
use crate::db::{AccountVaultValue, schema};
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

pub(crate) fn select_account_vault_assets(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    block_from: BlockNumber,
    block_to: BlockNumber,
) -> Result<(BlockNumber, Vec<AccountVaultValue>), DatabaseError> {
    use schema::account_vault_assets as t;

    // SELECT block_num, faucet_id, delta
    // FROM account_fungible_asset_deltas
    // WHERE account_id = ?
    //   AND block_num >= ?
    //   AND block_num <= ?
    // ORDER BY block_num ASC, faucet_id ASC
    // LIMIT ROW_LIMIT;

    const MAX_PAYLOAD_BYTES: usize = 5 * 1024 * 1024;
    const ROW_OVERHEAD_BYTES: usize = core::mem::size_of::<Word>() * 2;
    const ROW_LIMIT: usize = (MAX_PAYLOAD_BYTES / ROW_OVERHEAD_BYTES) + 1;
    const ROW_LIMIT_SQL: i64 = ROW_LIMIT as i64;

    if !account_id.is_public() {
        return Err(DatabaseError::AccountNotPublic(account_id));
    }

    if block_from > block_to {
        return Err(DatabaseError::InvalidBlockRange { from: block_from, to: block_to });
    }

    let mut raw: Vec<(i64, Vec<u8>, Option<Vec<u8>>)> =
        SelectDsl::select(t::table, (t::block_num, t::vault_key, t::asset))
            .filter(
                t::account_id
                    .eq(account_id.to_bytes())
                    .and(t::block_num.ge(block_from.to_raw_sql()))
                    .and(
                        // include rows exactly at block_to OR latest rows <= block_to
                        t::block_num.eq(block_to.to_raw_sql()).or(t::is_latest_update
                            .eq(true)
                            .and(t::block_num.le(block_to.to_raw_sql()))),
                    ),
            )
            .order(t::block_num.asc())
            .limit(ROW_LIMIT_SQL)
            .load::<(i64, Vec<u8>, Option<Vec<u8>>)>(conn)?;

    // Discard the last block in the response (assumes more than one block may be present)
    let mut last_block_included = block_to.checked_sub(1).unwrap_or_default();
    if raw.len() == ROW_LIMIT {
        // NOTE: If the query contains at least one more row than the amount of storage map updates
        // allowed in a single block for an account, then the response is guaranteed to have at
        // least two blocks
        if let Some(last_block_num) = raw.last().map(|r| r.0) {
            raw.retain(|(row_block_num, ..)| *row_block_num != last_block_num);
            last_block_included = BlockNumber::from_raw_sql(last_block_num.saturating_sub(1))?;
        }
    }

    let values = raw
        .into_iter()
        .map(|(block_num, vault_key, asset)| {
            Ok(AccountVaultValue {
                block_num: u32::try_from(block_num)
                    .map_err(|e| DatabaseError::conversiont_from_sql::<u32, _, _>(e))?
                    .into(),
                vault_key: Word::read_from_bytes(&vault_key)?,
                asset: asset.map(|b| Asset::read_from_bytes(&b)).transpose()?,
            })
        })
        .collect::<Result<Vec<_>, DatabaseError>>()?;

    Ok((last_block_included, values))
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageMapValue {
    pub slot_index: u8,
    pub key: Word,
    pub value: Word,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageMapValuesPage {
    /// Highest block number included in `rows`. If the page is empty, this will be `block_from`.
    pub last_block_included: BlockNumber,
    /// Storage map values
    pub values: Vec<StorageMapValue>,
}

impl StorageMapValue {
    pub fn from_raw_row(row: (i64, i32, Vec<u8>, Vec<u8>)) -> Result<Self, DatabaseError> {
        let (_, slot_index, key, value) = row;
        Ok(Self {
            slot_index: raw_sql_to_slot(slot_index),
            key: Word::read_from_bytes(&key)?,
            value: Word::read_from_bytes(&value)?,
        })
    }
}

/// Select account storage map values from the DB using the given [`SqliteConnection`].
///
/// # Returns
///
/// A vector of tuples containing `(slot, key, value, is_latest_update)` for the given account.
/// Each row contains one of:
///
/// - the historical value for a slot and key specifically on block `block_to`
/// - the latest updated value for the slot and key combination, alongside the block number in which
///   it was updated
pub(crate) fn select_account_storage_map_values(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    block_from: BlockNumber,
    block_to: BlockNumber,
) -> Result<StorageMapValuesPage, DatabaseError> {
    use schema::account_storage_map_values as t;

    // SELECT
    //     block_num,
    //     slot,
    //     key,
    //     value
    // FROM
    //     account_storage_map_values
    // WHERE
    //     account_id = ?1
    //     AND block_num >= ?2
    //     AND (block_num = ?3 OR (is_latest_update = 1 AND block_num <= ?3))
    // ORDER BY
    //     block_num ASC
    // LIMIT
    //     :row_limit;

    // TODO: These limits should be given by the protocol.
    // See miden-base/issues/1770 for more details
    pub const MAX_PAYLOAD_BYTES: usize = 2 * 1024 * 1024; // 2 MB
    pub const ROW_OVERHEAD_BYTES: usize = size_of::<Word>() + size_of::<Word>() + size_of::<u8>(); // key + value + slot_idx
    pub const ROW_LIMIT: usize = (MAX_PAYLOAD_BYTES / ROW_OVERHEAD_BYTES) + 1;

    if !account_id.is_public() {
        return Err(DatabaseError::AccountNotPublic(account_id));
    }

    if block_from > block_to {
        return Err(DatabaseError::InvalidBlockRange { from: block_from, to: block_to });
    }

    let raw: Vec<(i64, i32, Vec<u8>, Vec<u8>)> =
        SelectDsl::select(t::table, (t::block_num, t::slot, t::key, t::value))
            .filter(
                t::account_id
                    .eq(account_id.to_bytes())
                    .and(t::block_num.ge(block_from.to_raw_sql()))
                    .and(t::block_num.eq(block_to.to_raw_sql()).or(
                        t::is_latest_update.eq(true).and(t::block_num.le(block_to.to_raw_sql())),
                    )),
            )
            .order(t::block_num.asc())
            .limit(i64::try_from(ROW_LIMIT).expect("limit fits within i64"))
            .load(conn)?;

    // Discard the last block in the response (assumes more than one block may be present)
    let (last_block_included, values) = if raw.len() == ROW_LIMIT {
        // NOTE: If the query contains at least one more row than the amount of storage map updates
        // allowed in a single block for an account, then the response is guaranteed to have at
        // least two blocks

        // SAFETY: we checked that the vector is not empty
        let &(last_block_num, ..) = raw.last().unwrap();
        let values = raw
            .into_iter()
            .filter(|(bn, ..)| *bn != last_block_num)
            .map(StorageMapValue::from_raw_row)
            .collect::<Result<Vec<_>, DatabaseError>>()?;

        (BlockNumber::from_raw_sql(last_block_num.saturating_sub(1))?, values)
    } else {
        (
            block_to,
            raw.into_iter().map(StorageMapValue::from_raw_row).collect::<Result<_, _>>()?,
        )
    };

    Ok(StorageMapValuesPage { last_block_included, values })
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = crate::db::schema::account_vault_updates)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct AccountVaultUpdateRaw {
    pub vault_key: Vec<u8>,
    pub asset: Option<Vec<u8>>,
    pub block_num: i64,
}

impl TryFrom<AccountVaultUpdateRaw> for AccountVaultValue {
    type Error = DatabaseError;

    fn try_from(raw: AccountVaultUpdateRaw) -> Result<Self, Self::Error> {
        let vault_key = Word::read_from_bytes(&raw.vault_key)?;
        let asset = raw.asset.map(|bytes| Asset::read_from_bytes(&bytes)).transpose()?;
        let block_num = BlockNumber::from_raw_sql(raw.block_num)?;

        Ok(AccountVaultValue { block_num, vault_key, asset })
    }
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
