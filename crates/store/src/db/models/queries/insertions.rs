use std::borrow::Cow;

use diesel::prelude::{AsChangeset, Insertable};
use diesel::query_dsl::methods::SelectDsl;
use diesel::query_dsl::{QueryDsl, RunQueryDsl};
use diesel::{
    BoolExpressionMethods,
    ExpressionMethods,
    JoinOnDsl,
    NullableExpressionMethods,
    SelectableHelper,
    SqliteConnection,
};
use miden_lib::utils::Serializable;
use miden_node_proto as proto;
use miden_objects::Word;
use miden_objects::account::delta::AccountUpdateDetails;
use miden_objects::account::{
    Account,
    AccountDelta,
    AccountId,
    NonFungibleDeltaAction,
    StorageSlot,
};
use miden_objects::asset::{Asset, FungibleAsset};
use miden_objects::block::{BlockAccountUpdate, BlockHeader, BlockNumber};
use miden_objects::note::Nullifier;
use miden_objects::transaction::OrderedTransactionHeaders;

use super::accounts::{AccountRaw, AccountWithCodeRaw};
use super::{DatabaseError, NoteRecord};
use crate::db::models::conv::{
    SqlTypeConvert,
    aux_to_raw_sql,
    execution_hint_to_raw_sql,
    idx_to_raw_sql,
    nonce_to_raw_sql,
    note_type_to_raw_sql,
    slot_to_raw_sql,
};
use crate::db::schema;

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

/// Insert an account vault asset row into the DB using the given [`SqliteConnection`].
///
/// This function will set `is_latest_update=true` for the new row and update any existing
/// row with the same `(account_id, faucet_id)` tuple to `is_latest_update=false`.
///
/// # Returns
///
/// The number of affected rows.
pub(crate) fn insert_account_vault_asset(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    block_num: BlockNumber,
    vault_key: Word,
    asset: Option<Asset>,
) -> Result<usize, DatabaseError> {
    let account_id = account_id.to_bytes();
    let vault_key = vault_key.to_bytes();
    let block_num = block_num.to_raw_sql();
    let asset = asset.map(|asset| asset.to_bytes());
    diesel::Connection::transaction(conn, |conn| {
        // First, update any existing rows with the same (account_id, faucet_id) to set
        // is_latest_update=false
        let update_count = diesel::update(schema::account_vault_assets::table)
            .filter(
                schema::account_vault_assets::account_id
                    .eq(&account_id)
                    .and(schema::account_vault_assets::vault_key.eq(&vault_key))
                    .and(schema::account_vault_assets::is_latest_update.eq(true)),
            )
            .set(schema::account_vault_assets::is_latest_update.eq(false))
            .execute(conn)?;

        // Insert the new latest row
        let insert_count = diesel::insert_into(schema::account_vault_assets::table)
            .values((
                schema::account_vault_assets::account_id.eq(&account_id),
                schema::account_vault_assets::block_num.eq(block_num),
                schema::account_vault_assets::vault_key.eq(&vault_key),
                schema::account_vault_assets::asset.eq(&asset),
                schema::account_vault_assets::is_latest_update.eq(true),
            ))
            .execute(conn)?;

        Ok(update_count + insert_count)
    })
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

/// Insert an account storage map value into the DB using the given [`SqliteConnection`].
///
/// This function will set `is_latest_update=true` for the new row and update any existing
/// row with the same `(account_id, slot, key)` tuple to `is_latest_update=false`.
///
/// # Returns
///
/// The number of affected rows.
pub(crate) fn insert_account_storage_map_value(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    block_num: BlockNumber,
    slot: u8,
    key: Word,
    value: Word,
) -> Result<usize, DatabaseError> {
    let account_id = account_id.to_bytes();
    let key = key.to_bytes();
    let value = value.to_bytes();
    let slot_idx = slot_to_raw_sql(slot);
    let block_num = block_num.to_raw_sql();

    diesel::Connection::transaction(conn, |conn| {
        // First, update any existing rows with the same (account_id, slot, key) to set
        // is_latest_update=false
        let update_count = diesel::update(schema::account_storage_map_values::table)
            .filter(
                schema::account_storage_map_values::account_id
                    .eq(&account_id)
                    .and(schema::account_storage_map_values::slot.eq(slot_idx))
                    .and(schema::account_storage_map_values::key.eq(&key))
                    .and(schema::account_storage_map_values::is_latest_update.eq(true)),
            )
            .set(schema::account_storage_map_values::is_latest_update.eq(false))
            .execute(conn)?;

        let insert_count = diesel::insert_into(schema::account_storage_map_values::table)
            .values((
                schema::account_storage_map_values::account_id.eq(&account_id),
                schema::account_storage_map_values::block_num.eq(block_num),
                schema::account_storage_map_values::slot.eq(slot_idx),
                schema::account_storage_map_values::key.eq(&key),
                schema::account_storage_map_values::value.eq(&value),
                schema::account_storage_map_values::is_latest_update.eq(true),
            ))
            .execute(conn)?;

        Ok(update_count + insert_count)
    })
}

/// Attention: Assumes the account details are NOT null! The schema explicitly allows this though!
#[allow(clippy::too_many_lines)]
pub(crate) fn upsert_accounts(
    conn: &mut SqliteConnection,
    accounts: &[BlockAccountUpdate],
    block_num: BlockNumber,
) -> Result<usize, DatabaseError> {
    use proto::domain::account::NetworkAccountPrefix;

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

        let full_account = match update.details() {
            AccountUpdateDetails::Private => None,
            AccountUpdateDetails::New(account) => {
                debug_assert_eq!(account_id, account.id());

                if account.commitment() != update.final_state_commitment() {
                    return Err(DatabaseError::AccountCommitmentsMismatch {
                        calculated: account.commitment(),
                        expected: update.final_state_commitment(),
                    });
                }

                for (slot_idx, slot) in account.storage().slots().iter().enumerate() {
                    match slot {
                        StorageSlot::Value(_) => {},
                        StorageSlot::Map(storage_map) => {
                            for (key, value) in storage_map.entries() {
                                // SAFETY: We can safely unwrap the conversion to u8 because
                                // accounts have a limit of 255 storage elements
                                insert_account_storage_map_value(
                                    conn,
                                    account_id,
                                    block_num,
                                    u8::try_from(slot_idx).unwrap(),
                                    *key,
                                    *value,
                                )?;
                            }
                        },
                    }
                }

                Some(Cow::Borrowed(account))
            },
            AccountUpdateDetails::Delta(delta) => {
                let mut rows = select_details_stmt(conn, account_id)?.into_iter();
                let Some(account) = rows.next() else {
                    return Err(DatabaseError::AccountNotFoundInDb(account_id));
                };

                for (&slot, map_delta) in delta.storage().maps() {
                    for (key, value) in map_delta.entries() {
                        insert_account_storage_map_value(
                            conn,
                            account_id,
                            block_num,
                            slot,
                            (*key).into(),
                            *value,
                        )?;
                    }
                }

                let account = apply_delta(account, delta, &update.final_state_commitment())?;

                // Update assets

                for (faucet_id, _) in delta.vault().fungible().iter() {
                    let current_amount = account.vault().get_balance(*faucet_id).unwrap();
                    let asset: Asset = FungibleAsset::new(*faucet_id, current_amount)?.into();
                    let asset_update_or_removal =
                        if current_amount == 0 { None } else { Some(asset) };

                    insert_account_vault_asset(
                        conn,
                        account.id(),
                        block_num,
                        asset.vault_key(),
                        asset_update_or_removal,
                    )?;
                }

                for (asset, delta_action) in delta.vault().non_fungible().iter() {
                    let asset_update = match delta_action {
                        NonFungibleDeltaAction::Add => Some(Asset::NonFungible(*asset)),
                        NonFungibleDeltaAction::Remove => None,
                    };
                    insert_account_vault_asset(
                        conn,
                        account.id(),
                        block_num,
                        asset.vault_key(),
                        asset_update,
                    )?;
                }

                Some(Cow::Owned(account))
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

#[derive(Debug, Clone, PartialEq, Insertable)]
#[diesel(table_name = schema::notes)]
pub struct NoteInsertRowRaw {
    pub committed_at: i64,

    pub batch_index: i32,
    pub note_index: i32, // index within batch

    pub note_id: Vec<u8>,

    pub note_type: i32,
    pub sender: Vec<u8>, // AccountId
    pub tag: i32,
    pub aux: i64,
    pub execution_hint: i64,

    pub consumed_at: Option<i64>,
    pub assets: Option<Vec<u8>>,
    pub inputs: Option<Vec<u8>>,
    pub serial_num: Option<Vec<u8>>,
    pub nullifier: Option<Vec<u8>>,
    pub script_root: Option<Vec<u8>>,
    pub execution_mode: i32,
    pub inclusion_path: Vec<u8>,
}

impl From<(NoteRecord, Option<Nullifier>)> for NoteInsertRowRaw {
    fn from((note, nullifier): (NoteRecord, Option<Nullifier>)) -> Self {
        Self {
            committed_at: note.block_num.to_raw_sql(),
            batch_index: idx_to_raw_sql(note.note_index.batch_idx()),
            note_index: idx_to_raw_sql(note.note_index.note_idx_in_batch()),
            note_id: note.note_id.to_bytes(),
            note_type: note_type_to_raw_sql(note.metadata.note_type() as u8),
            sender: note.metadata.sender().to_bytes(),
            tag: note.metadata.tag().to_raw_sql(),
            execution_mode: note.metadata.tag().execution_mode().to_raw_sql(),
            aux: aux_to_raw_sql(note.metadata.aux()),
            execution_hint: execution_hint_to_raw_sql(note.metadata.execution_hint().into()),
            inclusion_path: note.inclusion_path.to_bytes(),
            consumed_at: None::<i64>, // New notes are always unconsumed.
            nullifier: nullifier.as_ref().map(Nullifier::to_bytes), /* Beware: `Option<T>` also implements `to_bytes`, but this is not what you want. */
            assets: note.details.as_ref().map(|d| d.assets().to_bytes()),
            inputs: note.details.as_ref().map(|d| d.inputs().to_bytes()),
            script_root: note.details.as_ref().map(|d| d.script().root().to_bytes()),
            serial_num: note.details.as_ref().map(|d| d.serial_num().to_bytes()),
        }
    }
}

#[derive(Insertable, Debug, Clone)]
#[diesel(table_name = schema::account_codes)]
pub(crate) struct AccountCodeRowInsert {
    pub(crate) code_commitment: Vec<u8>,
    pub(crate) code: Vec<u8>,
}

#[derive(Insertable, AsChangeset, Debug, Clone)]
#[diesel(table_name = schema::accounts)]
pub(crate) struct AccountRowInsert {
    pub(crate) account_id: Vec<u8>,
    pub(crate) network_account_id_prefix: Option<i64>,
    pub(crate) block_num: i64,
    pub(crate) account_commitment: Vec<u8>,
    pub(crate) code_commitment: Option<Vec<u8>>,
    pub(crate) storage: Option<Vec<u8>>,
    pub(crate) vault: Option<Vec<u8>>,
    pub(crate) nonce: Option<i64>,
}
