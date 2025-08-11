use super::*;

/// Insert a [`BlockHeader`] to the DB using the given [`SqliteConnection`].
///
/// # Returns
///
/// The number of affected rows.
///
/// # Note
///
/// The [`SqliteConnection`] object is not consumed. It's up to the caller to commit or rollback the
/// transaction
pub(crate) fn insert_block_header(
    conn: &mut SqliteConnection,
    block_header: &BlockHeader,
) -> Result<usize, DatabaseError> {
    let count = diesel::insert_into(schema::block_headers::table)
        .values(&[(
            schema::block_headers::block_num.eq(block_header.block_num().to_raw_sql()),
            schema::block_headers::block_header.eq(block_header.to_bytes()),
        )])
        .execute(conn)?;
    Ok(count)
}

/// Deserializes account and applies account delta.
pub(crate) fn apply_delta(
    mut account: Account,
    delta: &AccountDelta,
    final_state_commitment: &Word,
) -> crate::db::Result<Account, DatabaseError> {
    account.apply_delta(delta)?;

    let actual_commitment = account.commitment();
    if &actual_commitment != final_state_commitment {
        return Err(DatabaseError::AccountCommitmentsMismatch {
            calculated: actual_commitment,
            expected: *final_state_commitment,
        });
    }

    Ok(account)
}

#[allow(clippy::too_many_lines)]
pub(crate) fn insert_account_delta(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    block_number: BlockNumber,
    delta: &AccountDelta,
) -> Result<(), DatabaseError> {
    fn insert_acc_delta_stmt(
        conn: &mut SqliteConnection,
        account_id: AccountId,
        block_num: BlockNumber,
        nonce: Felt,
    ) -> Result<usize, DatabaseError> {
        let count = diesel::insert_into(schema::account_deltas::table)
            .values(&[(
                schema::account_deltas::account_id.eq(account_id.to_bytes()),
                schema::account_deltas::block_num.eq(block_num.to_raw_sql()),
                schema::account_deltas::nonce.eq(nonce_to_raw_sql(nonce)),
            )])
            .execute(conn)?;
        Ok(count)
    }

    fn insert_slot_update_stmt(
        conn: &mut SqliteConnection,
        account_id: AccountId,
        block_num: BlockNumber,
        slot: u8,
        value: Vec<u8>,
    ) -> Result<usize, DatabaseError> {
        let count = diesel::insert_into(schema::account_storage_slot_updates::table)
            .values(&[(
                schema::account_storage_slot_updates::account_id.eq(account_id.to_bytes()),
                schema::account_storage_slot_updates::block_num.eq(block_num.to_raw_sql()),
                schema::account_storage_slot_updates::slot.eq(slot_to_raw_sql(slot)),
                schema::account_storage_slot_updates::value.eq(value),
            )])
            .execute(conn)?;
        Ok(count)
    }

    fn insert_storage_map_update_stmt(
        conn2: &mut SqliteConnection,
        account_id: AccountId,
        block_num: BlockNumber,
        slot: u8,
        key: Vec<u8>,
        value: Vec<u8>,
    ) -> Result<usize, DatabaseError> {
        let count = diesel::insert_into(schema::account_storage_map_updates::table)
            .values(&[(
                schema::account_storage_map_updates::account_id.eq(account_id.to_bytes()),
                schema::account_storage_map_updates::block_num.eq(block_num.to_raw_sql()),
                schema::account_storage_map_updates::slot.eq(slot_to_raw_sql(slot)),
                schema::account_storage_map_updates::key.eq(key),
                schema::account_storage_map_updates::value.eq(value),
            )])
            .execute(conn2)?;
        Ok(count)
    }

    fn insert_fungible_asset_delta_stmt(
        conn2: &mut SqliteConnection,
        account_id: AccountId,
        block_num: BlockNumber,
        faucet_id: Vec<u8>,
        delta: i64,
    ) -> Result<usize, DatabaseError> {
        let count = diesel::insert_into(schema::account_fungible_asset_deltas::table)
            .values(&[(
                schema::account_fungible_asset_deltas::account_id.eq(account_id.to_bytes()),
                schema::account_fungible_asset_deltas::block_num.eq(block_num.to_raw_sql()),
                schema::account_fungible_asset_deltas::faucet_id.eq(faucet_id),
                schema::account_fungible_asset_deltas::delta.eq(fungible_delta_to_raw_sql(delta)),
            )])
            .execute(conn2)?;
        Ok(count)
    }

    pub(crate) fn insert_non_fungible_asset_update_stmt(
        conn2: &mut SqliteConnection,
        account_id: AccountId,
        block_num: BlockNumber,
        vault_key: Vec<u8>,
        is_remove: bool,
    ) -> Result<usize, DatabaseError> {
        let count = diesel::insert_into(schema::account_non_fungible_asset_updates::table)
            .values(&[(
                schema::account_non_fungible_asset_updates::account_id.eq(account_id.to_bytes()),
                schema::account_non_fungible_asset_updates::block_num.eq(block_num.to_raw_sql()),
                schema::account_non_fungible_asset_updates::vault_key.eq(vault_key),
                schema::account_non_fungible_asset_updates::is_remove.eq(is_remove),
            )])
            .execute(conn2)?;
        Ok(count)
    }

    insert_acc_delta_stmt(conn, account_id, block_number, delta.nonce_delta())?;

    for (&slot, value) in delta.storage().values() {
        insert_slot_update_stmt(conn, account_id, block_number, slot, value.to_bytes())?;
    }

    for (&slot, map_delta) in delta.storage().maps() {
        for (key, value) in map_delta.entries() {
            insert_storage_map_update_stmt(
                conn,
                account_id,
                block_number,
                slot,
                key.to_bytes(),
                value.to_bytes(),
            )?;
        }
    }

    for (&faucet_id, &delta) in delta.vault().fungible().iter() {
        insert_fungible_asset_delta_stmt(
            conn,
            account_id,
            block_number,
            faucet_id.to_bytes(),
            delta,
        )?;
    }

    for (&asset, action) in delta.vault().non_fungible().iter() {
        // TODO consider moving this out into a `TryFrom<u8/bool>` and `Into<u8/bool>`
        // respectively.
        let is_remove = match action {
            NonFungibleDeltaAction::Add => false,
            NonFungibleDeltaAction::Remove => true,
        };
        insert_non_fungible_asset_update_stmt(
            conn,
            account_id,
            block_number,
            asset.to_bytes(),
            is_remove,
        )?;
    }

    Ok(())
}

/// Builds an [`AccountDelta`] from the given [`Account`].
///
/// This function should only be used when inserting a new account into the DB.The returned delta
/// could be thought of as the difference between an "empty transaction" and the it's initial state.
fn build_insert_delta(account: &Account) -> Result<AccountDelta, DatabaseError> {
    // Build storage delta
    let mut values = BTreeMap::new();
    let mut maps = BTreeMap::new();
    for (slot_idx, slot) in account.storage().clone().into_iter().enumerate() {
        let slot_idx: u8 = slot_idx.try_into().expect("slot index must fit into `u8`");

        match slot {
            StorageSlot::Value(value) => {
                values.insert(slot_idx, value);
            },

            StorageSlot::Map(map) => {
                maps.insert(slot_idx, map.into());
            },
        }
    }
    let storage_delta = AccountStorageDelta::from_parts(values, maps)?;

    // Build vault delta
    let mut fungible = BTreeMap::new();
    let mut non_fungible = BTreeMap::new();
    for asset in account.vault().assets() {
        match asset {
            Asset::Fungible(asset) => {
                fungible.insert(
                    asset.faucet_id(),
                    asset
                        .amount()
                        .try_into()
                        .expect("asset amount should be at most i64::MAX by construction"),
                );
            },

            Asset::NonFungible(asset) => {
                non_fungible.insert(LexicographicWord::new(asset), NonFungibleDeltaAction::Add);
            },
        }
    }

    let vault_delta = AccountVaultDelta::new(
        FungibleAssetDelta::new(fungible)?,
        NonFungibleAssetDelta::new(non_fungible),
    );

    Ok(AccountDelta::new(account.id(), storage_delta, vault_delta, account.nonce())?)
}

/// Attention: Assumes the account details are NOT null! The schema explicitly allows this though!
#[allow(clippy::too_many_lines)]
pub(crate) fn upsert_accounts(
    conn: &mut SqliteConnection,
    accounts: &[BlockAccountUpdate],
    block_num: BlockNumber,
) -> Result<usize, DatabaseError> {
    fn select_details_stmt(
        conn: &mut SqliteConnection,
        account_id: AccountId,
    ) -> Result<Vec<Account>, DatabaseError> {
        let account_id = account_id.to_bytes();
        let accounts = SelectDsl::select(
            schema::accounts::table.left_join(
                schema::account_codes::table.on(schema::accounts::code_commitment
                    .eq(schema::account_codes::code_commitment.nullable())),
            ),
            (AccountRaw::as_select(), schema::account_codes::code.nullable()),
        )
        .filter(schema::accounts::account_id.eq(account_id))
        .get_results::<(AccountRaw, Option<Vec<u8>>)>(conn)?;

        // SELECT .. FROM accounts LEFT JOIN account_codes
        // ON accounts.code_commitment == account_codes.code_commitment

        let accounts = Result::from_iter(accounts.into_iter().filter_map(|x| {
            let account_with_code = AccountWithCodeRaw::from(x);
            account_with_code.try_into().transpose()
        }))?;
        Ok(accounts)
    }

    let mut count = 0;
    for update in accounts {
        let account_id = update.account_id();
        // Extract the 30-bit prefix to provide easy look ups for NTB
        // Do not store prefix for accounts that are not network
        let network_account_id_prefix = if account_id.is_network() {
            Some(NetworkAccountPrefix::try_from(account_id)?)
        } else {
            None
        };

        let (full_account, insert_delta) = match update.details() {
            AccountUpdateDetails::Private => (None, None),
            AccountUpdateDetails::New(account) => {
                debug_assert_eq!(account_id, account.id());

                if account.commitment() != update.final_state_commitment() {
                    return Err(DatabaseError::AccountCommitmentsMismatch {
                        calculated: account.commitment(),
                        expected: update.final_state_commitment(),
                    });
                }

                let insert_delta = build_insert_delta(account)?;

                (Some(Cow::Borrowed(account)), Some(Cow::Owned(insert_delta)))
            },
            AccountUpdateDetails::Delta(delta) => {
                let mut rows = select_details_stmt(conn, account_id)?.into_iter();
                let Some(account) = rows.next() else {
                    return Err(DatabaseError::AccountNotFoundInDb(account_id));
                };

                let account = apply_delta(account, delta, &update.final_state_commitment())?;

                (Some(Cow::Owned(account)), Some(Cow::Borrowed(delta)))
            },
        };

        if let Some(code) = full_account.as_ref().map(|account| account.code()) {
            let code_value = AccountCodeRowInsert {
                code_commitment: code.commitment().to_bytes(),
                code: code.to_bytes(),
            };
            diesel::insert_into(schema::account_codes::table)
                .values(&code_value)
                .on_conflict(schema::account_codes::code_commitment)
                .do_nothing()
                .execute(conn)?;
        }

        let account_value = AccountRowInsert {
            account_id: account_id.to_bytes(),
            network_account_id_prefix: network_account_id_prefix
                .map(NetworkAccountPrefix::to_raw_sql),
            account_commitment: update.final_state_commitment().to_bytes(),
            block_num: block_num.to_raw_sql(),
            nonce: full_account.as_ref().map(|account| nonce_to_raw_sql(account.nonce())),
            storage: full_account.as_ref().map(|account| account.storage().to_bytes()),
            vault: full_account.as_ref().map(|account| account.vault().to_bytes()),
            code_commitment: full_account
                .as_ref()
                .map(|account| account.code().commitment().to_bytes()),
        };

        let v = account_value.clone();
        let inserted = diesel::insert_into(schema::accounts::table)
            .values(&v)
            .on_conflict(schema::accounts::account_id)
            .do_update()
            .set(account_value)
            .execute(conn)?;

        debug_assert_eq!(inserted, 1);

        if let Some(delta) = insert_delta {
            insert_account_delta(conn, account_id, block_num, &delta)?;
        }

        count += inserted;
    }

    Ok(count)
}

/// Insert notes to the DB using the given [`SqliteConnection`]. Public notes should also have a
/// nullifier.
///
/// # Returns
///
/// The number of affected rows.
///
/// # Note
///
/// The [`SqliteConnection`] object is not consumed. It's up to the caller to commit or rollback the
/// transaction.
pub(crate) fn insert_notes(
    conn: &mut SqliteConnection,
    notes: &[(NoteRecord, Option<Nullifier>)],
) -> Result<usize, DatabaseError> {
    let count = diesel::insert_into(schema::notes::table)
        .values(Vec::from_iter(
            notes
                .iter()
                .map(|(note, nullifier)| NoteInsertRowRaw::from((note.clone(), *nullifier))),
        ))
        .execute(conn)?;
    Ok(count)
}

/// Insert scripts to the DB using the given [`SqliteConnection`]. It inserts the scripts held by
/// the notes passed as parameter. If the script root already exists in the DB, it will be ignored.
///
/// # Returns
///
/// The number of affected rows.
///
/// # Note
///
/// The [`SqliteConnection`] object is not consumed. It's up to the caller to commit or rollback the
/// transaction.
pub(crate) fn insert_scripts<'a>(
    conn: &mut SqliteConnection,
    notes: impl IntoIterator<Item = &'a NoteRecord>,
) -> Result<usize, DatabaseError> {
    let values = Vec::from_iter(notes.into_iter().filter_map(|note| {
        let note_details = note.details.as_ref()?;
        Some((
            schema::note_scripts::script_root.eq(note_details.script().root().to_bytes()),
            schema::note_scripts::script.eq(note_details.script().to_bytes()),
        ))
    }));
    let count = diesel::insert_or_ignore_into(schema::note_scripts::table)
        .values(values)
        .execute(conn)?;

    Ok(count)
}

/// Insert transactions to the DB using the given [`SqliteConnection`].
///
/// # Returns
///
/// The number of affected rows.
///
/// # Note
///
/// The [`SqliteConnection`] object is not consumed. It's up to the caller to commit or rollback the
/// transaction.
pub(crate) fn insert_transactions(
    conn: &mut SqliteConnection,
    block_num: BlockNumber,
    transactions: &OrderedTransactionHeaders,
) -> Result<usize, DatabaseError> {
    #[allow(clippy::into_iter_on_ref)] // false positive
    let count = diesel::insert_into(schema::transactions::table)
        .values(Vec::from_iter(transactions.as_slice().into_iter().map(|tx| {
            (
                schema::transactions::transaction_id.eq(tx.id().to_bytes()),
                schema::transactions::account_id.eq(tx.account_id().to_bytes()),
                schema::transactions::block_num.eq(block_num.to_raw_sql()),
            )
        })))
        .execute(conn)?;
    Ok(count)
}
