use super::*;

/// Select the latest account details by account id from the DB using the given
/// [`SqliteConnection`].
///
/// # Returns
///
/// The latest account details, or an error.
pub(crate) fn select_account(
    conn: &mut SqliteConnection,
    account_id: AccountId,
) -> Result<AccountInfo, DatabaseError> {
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
    let info: AccountInfo = AccountWithCodeRaw::from(raw).try_into()?;
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

        match is_remove {
            false => non_fungible_delta.add(asset)?,
            true => non_fungible_delta.remove(asset)?,
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
