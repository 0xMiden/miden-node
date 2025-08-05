#![allow(
    clippy::needless_pass_by_value,
    reason = "The parent scope does own it, passing by value avoids additional boilerplate"
)]

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
};

use diesel::{
    JoinOnDsl, NullableExpressionMethods, OptionalExtension, SqliteConnection, alias,
    prelude::Queryable, query_dsl::methods::SelectDsl,
};
use miden_lib::utils::{Deserializable, Serializable};
use miden_node_proto::domain::account::{AccountInfo, AccountSummary, NetworkAccountPrefix};
use miden_node_utils::limiter::{
    QueryParamAccountIdLimit, QueryParamBlockLimit, QueryParamLimiter, QueryParamNoteIdLimit,
    QueryParamNoteTagLimit, QueryParamNullifierLimit, QueryParamNullifierPrefixLimit,
};
use miden_objects::{
    Felt, LexicographicWord, Word,
    account::{
        Account, AccountDelta, AccountId, AccountStorageDelta, AccountVaultDelta,
        FungibleAssetDelta, NonFungibleAssetDelta, NonFungibleDeltaAction, StorageMapDelta,
        StorageSlot, delta::AccountUpdateDetails,
    },
    asset::{Asset, NonFungibleAsset},
    block::{BlockAccountUpdate, BlockHeader, BlockNoteIndex, BlockNumber},
    crypto::merkle::SparseMerklePath,
    note::{NoteExecutionMode, NoteId, NoteInclusionProof, Nullifier},
    transaction::OrderedTransactionHeaders,
};

use super::{
    super::models, BoolExpressionMethods, DatabaseError, NoteSyncRecordRawRow, QueryDsl,
    RunQueryDsl, SelectableHelper,
};
use crate::{
    db::{
        NoteRecord, NoteSyncRecord, NoteSyncUpdate, NullifierInfo, Page, StateSyncUpdate,
        TransactionSummary,
        models::{
            AccountRaw, AccountSummaryRaw, BigIntSum, ExpressionMethods, NoteInsertRowRaw,
            NoteRecordRaw, NoteRecordWithScriptRaw, TransactionSummaryRaw,
            conv::{
                SqlTypeConvert, fungible_delta_to_raw_sql, nonce_to_raw_sql,
                nullifier_prefix_to_raw_sql, raw_sql_to_idx, raw_sql_to_nonce, raw_sql_to_slot,
                slot_to_raw_sql,
            },
            deserialize_raw_vec, get_nullifier_prefix, serialize_vec, sql_sum_into,
            vec_raw_try_into,
        },
        schema,
    },
    errors::{NoteSyncError, StateSyncError},
};

/// Select notes matching the tags and account IDs search criteria using the given [Connection].
///
/// # Returns
///
/// All matching notes from the first block greater than `block_num` containing a matching note.
/// A note is considered a match if it has any of the given tags, or if its sender is one of the
/// given account IDs. If no matching notes are found at all, then an empty vector is returned.
///
/// # Note
///
/// This method returns notes from a single block. To fetch all notes up to the chain tip,
/// multiple requests are necessary.
pub(crate) fn select_notes_since_block_by_tag_and_sender(
    conn: &mut SqliteConnection,
    start_block_number: BlockNumber,
    account_ids: &[AccountId],
    note_tags: &[u32],
) -> Result<Vec<NoteSyncRecord>, DatabaseError> {
    QueryParamAccountIdLimit::check(account_ids.len())?;
    QueryParamNoteTagLimit::check(note_tags.len())?;
    // SELECT
    //     block_num,
    //     batch_index,
    //     note_index,
    //     note_id,
    //     note_type,
    //     sender,
    //     tag,
    //     aux,
    //     execution_hint,
    //     inclusion_path
    // FROM
    //     notes
    // WHERE
    //     -- find the next block which contains at least one note with a matching tag or sender
    //     block_num = (
    //         SELECT
    //             block_num
    //         FROM
    //             notes
    //         WHERE
    //             (tag IN rarray(?1) OR sender IN rarray(?2)) AND
    //             block_num > ?3
    //         ORDER BY
    //             block_num ASC
    //     LIMIT 1) AND
    //     -- filter the block's notes and return only the ones matching the requested tags or
    // senders     (tag IN rarray(?1) OR sender IN rarray(?2))

    let desired_note_tags = Vec::from_iter(note_tags.iter().map(|tag| *tag as i32));
    let desired_senders = serialize_vec(account_ids.iter());

    let start_block_num = start_block_number.to_raw_sql();

    // find block_num: select notes since block by tag and sender
    let Some(desired_block_num): Option<i64> =
        SelectDsl::select(schema::notes::table, schema::notes::block_num)
            .filter(
                schema::notes::tag
                    .eq_any(&desired_note_tags[..])
                    .or(schema::notes::sender.eq_any(&desired_senders[..])),
            )
            .filter(schema::notes::block_num.gt(start_block_num))
            .order_by(schema::notes::block_num.asc())
            .limit(1)
            .get_result(conn)
            .optional()?
    else {
        return Ok(Vec::new());
    };

    let notes = SelectDsl::select(schema::notes::table, NoteSyncRecordRawRow::as_select())
            // find the next block which contains at least one note with a matching tag or sender
            .filter(schema::notes::block_num.eq(
                &desired_block_num
            ))
            // filter the block's notes and return only the ones matching the requested tags or senders
            .filter(
                schema::notes::tag
                .eq_any(&desired_note_tags)
                .or(
                    schema::notes::sender
                    .eq_any(&desired_senders)
                )
            )
            .get_results::<NoteSyncRecordRawRow>(conn)
            .map_err(DatabaseError::from)?;

    vec_raw_try_into(notes)
}

/// Select a [`BlockHeader`] from the DB by its `block_num` using the given [Connection].
///
/// # Returns
///
/// When `block_number` is [None], the latest block header is returned. Otherwise, the block with
/// the given block height is returned.
pub(crate) fn select_block_header_by_block_num(
    conn: &mut SqliteConnection,
    maybe_block_number: Option<BlockNumber>,
) -> Result<Option<BlockHeader>, DatabaseError> {
    let sel = SelectDsl::select(schema::block_headers::table, models::BlockHeaderRaw::as_select());
    let row = if let Some(block_number) = maybe_block_number {
        // SELECT block_header FROM block_headers WHERE block_num = ?1
        sel.filter(schema::block_headers::block_num.eq(block_number.to_raw_sql()))
            .get_result::<models::BlockHeaderRaw>(conn)
            .optional()?
        // invariant: only one block exists with the given block header, so the length is
        // always zero or one
    } else {
        // SELECT block_header FROM block_headers ORDER BY block_num DESC LIMIT 1
        sel.order(schema::block_headers::block_num.desc())
            .limit(1)
            .get_result::<models::BlockHeaderRaw>(conn)
            .optional()?
    };
    row.map(std::convert::TryInto::try_into).transpose()
}

/// Select note inclusion proofs matching the `NoteId`, using the given [Connection].
///
/// # Returns
///
/// - Empty map if no matching `note`.
/// - Otherwise, note inclusion proofs, which `note_id` matches the `NoteId` as bytes.
pub(crate) fn select_note_inclusion_proofs(
    conn: &mut SqliteConnection,
    note_ids: &BTreeSet<NoteId>,
) -> Result<BTreeMap<NoteId, NoteInclusionProof>, DatabaseError> {
    QueryParamNoteIdLimit::check(note_ids.len())?;

    // SELECT
    //     block_num,
    //     note_id,
    //     batch_index,
    //     note_index,
    //     inclusion_path
    // FROM
    //     notes
    // WHERE
    //     note_id IN rarray(?1)
    // ORDER BY
    //     block_num ASC
    let noted_ids_serialized = serialize_vec(note_ids.iter());

    let raw_notes = SelectDsl::select(
        schema::notes::table,
        (
            schema::notes::block_num,
            schema::notes::note_id,
            schema::notes::batch_index,
            schema::notes::note_index,
            schema::notes::inclusion_path,
        ),
    )
    .filter(schema::notes::note_id.eq_any(noted_ids_serialized))
    .order_by(schema::notes::block_num.asc())
    .load::<(i64, Vec<u8>, i32, i32, Vec<u8>)>(conn)?;

    Result::<BTreeMap<_, _>, _>::from_iter(raw_notes.iter().map(
        |(block_num, note_id, batch_index, note_index, merkle_path)| {
            let note_id = NoteId::read_from_bytes(&note_id[..])?;
            let block_num = BlockNumber::from_raw_sql(*block_num)?;
            let node_index_in_block =
                BlockNoteIndex::new(raw_sql_to_idx(*batch_index), raw_sql_to_idx(*note_index))
                    .expect("batch and note index from DB should be valid")
                    .leaf_index_value();
            let merkle_path = SparseMerklePath::read_from_bytes(&merkle_path[..])?;
            let proof = NoteInclusionProof::new(block_num, node_index_in_block, merkle_path)?;
            Ok((note_id, proof))
        },
    ))
}

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
        is_remove: i32,
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
            NonFungibleDeltaAction::Add => 0,
            NonFungibleDeltaAction::Remove => 1,
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
        let account_details_serialized =
            SelectDsl::select(schema::accounts::table, schema::accounts::details.assume_not_null())
                .filter(schema::accounts::account_id.eq(account_id))
                .get_results::<Vec<u8>>(conn)?;
        let accounts = deserialize_raw_vec::<_, Account>(account_details_serialized)?;
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

        let val = (
            schema::accounts::account_id.eq(account_id.to_bytes()),
            schema::accounts::network_account_id_prefix
                .eq(network_account_id_prefix.map(SqlTypeConvert::to_raw_sql)),
            schema::accounts::account_commitment.eq(update.final_state_commitment().to_bytes()),
            schema::accounts::block_num.eq(block_num.to_raw_sql()),
            schema::accounts::details.eq(full_account.as_ref().map(|account| account.to_bytes())),
        );
        let v = val.clone();
        let inserted = diesel::insert_into(schema::accounts::table)
            .values(&v)
            .on_conflict(schema::accounts::account_id)
            .do_update()
            .set(val)
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

pub(crate) fn select_notes_by_id(
    conn: &mut SqliteConnection,
    note_ids: &[NoteId],
) -> Result<Vec<NoteRecord>, DatabaseError> {
    let note_ids = serialize_vec(note_ids);
    // SELECT {}
    // FROM notes
    // LEFT JOIN note_scripts ON notes.script_root = note_scripts.script_root
    // WHERE note_id IN rarray(?1),

    let q = schema::notes::table
        .left_join(
            schema::note_scripts::table
                .on(schema::notes::script_root.eq(schema::note_scripts::script_root.nullable())),
        )
        .filter(schema::notes::note_id.eq_any(&note_ids));
    let raw: Vec<_> =
        SelectDsl::select(q, (NoteRecordRaw::as_select(), schema::note_scripts::script.nullable()))
            .load::<(NoteRecordRaw, Option<Vec<u8>>)>(conn)?;
    let records = vec_raw_try_into::<NoteRecord, NoteRecordWithScriptRaw>(
        raw.into_iter().map(NoteRecordWithScriptRaw::from),
    )?;
    Ok(records)
}

/// Select all notes from the DB using the given [`SqliteConnection`].
///
///
/// # Returns
///
/// A vector with notes, or an error.
#[cfg(test)]
pub(crate) fn select_all_notes(
    conn: &mut SqliteConnection,
) -> Result<Vec<NoteRecord>, DatabaseError> {
    // SELECT {cols}
    // FROM notes
    // LEFT JOIN note_scripts ON notes.script_root = note_scripts.script_root
    // ORDER BY block_num ASC
    let q = schema::notes::table.left_join(
        schema::note_scripts::table
            .on(schema::notes::script_root.eq(schema::note_scripts::script_root.nullable())),
    );
    let raw: Vec<_> =
        SelectDsl::select(q, (NoteRecordRaw::as_select(), schema::note_scripts::script.nullable()))
            .order(schema::notes::block_num.asc())
            .load::<(NoteRecordRaw, Option<Vec<u8>>)>(conn)?;
    let records = vec_raw_try_into::<NoteRecord, NoteRecordWithScriptRaw>(
        raw.into_iter().map(NoteRecordWithScriptRaw::from),
    )?;
    Ok(records)
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
        .set(schema::notes::consumed_block_num.eq(Some(block_num.to_raw_sql())))
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

/// Select the latest account details by account id from the DB using the given [Connection].
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
    let info = SelectDsl::select(schema::accounts::table, AccountRaw::as_select())
        .filter(schema::accounts::account_id.eq(account_id.to_bytes()))
        .get_result::<models::AccountRaw>(conn)
        .optional()?
        .ok_or(DatabaseError::AccountNotFoundInDb(account_id))?;
    let info = info.try_into()?;
    Ok(info)
}

// TODO: Handle account prefix collision in a more robust way
/// Select the latest account details by account ID prefix from the DB using the given [Connection]
/// This method is meant to be used by the network transaction builder. Because network notes get
/// matched through accounts through the account's 30-bit prefix, it is possible that multiple
/// accounts match against a single prefix. In this scenario, the first account is returned.
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
    let maybe_info = SelectDsl::select(schema::accounts::table, AccountRaw::as_select())
        .filter(schema::accounts::network_account_id_prefix.eq(Some(i64::from(id_prefix))))
        .get_result::<AccountRaw>(conn)
        .optional()
        .map_err(DatabaseError::Diesel)?;

    let result: Result<Option<AccountInfo>, DatabaseError> =
        maybe_info.map(std::convert::TryInto::<AccountInfo>::try_into).transpose();

    result
}

/// Select all account commitments from the DB using the given [Connection].
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

/// Returns a paginated batch of network notes that have not yet been consumed.
///
/// # Returns
///
/// A set of unconsumed network notes with maximum length of `size` and the page to get
/// the next set.
//
// Attention: uses the _implicit_ column `rowid`, which requires to use a few raw SQL nugget
// statements
#[allow(
    clippy::cast_sign_loss,
    reason = "We need custom SQL statements which has given types that we need to convert"
)]
pub(crate) fn unconsumed_network_notes(
    conn: &mut SqliteConnection,
    mut page: Page,
) -> Result<(Vec<NoteRecord>, Page), DatabaseError> {
    assert_eq!(
        NoteExecutionMode::Network as u8,
        0,
        "Hardcoded execution value must match query"
    );

    let rowid_sel = diesel::dsl::sql::<diesel::sql_types::BigInt>("notes.rowid");
    let rowid_sel_ge =
        diesel::dsl::sql::<diesel::sql_types::Bool>("notes.rowid >= ")
            .bind::<diesel::sql_types::BigInt, i64>(page.token.unwrap_or_default() as i64);

    // SELECT {}, rowid
    // FROM notes
    // LEFT JOIN note_scripts ON notes.script_root = note_scripts.script_root
    // WHERE
    //     execution_mode = 0 AND consumed_block_num = NULL AND rowid >= ?
    // ORDER BY rowid
    // LIMIT ?
    #[allow(
        clippy::items_after_statements,
        reason = "It's only relevant for a single call function"
    )]
    type RawLoadedTuple = (
        NoteRecordRaw,
        Option<Vec<u8>>, // script
        i64,             // rowid (from sql::<BigInt>("notes.rowid"))
    );

    #[allow(
        clippy::items_after_statements,
        reason = "It's only relevant for a single call function"
    )]
    fn split_into_raw_note_record_and_implicit_row_id(
        tuple: RawLoadedTuple,
    ) -> (NoteRecordWithScriptRaw, i64) {
        let (note, script, row) = tuple;
        let combined = NoteRecordWithScriptRaw::from((note, script));
        (combined, row)
    }

    let raw = SelectDsl::select(
        schema::notes::table.left_join(
            schema::note_scripts::table
                .on(schema::notes::script_root.eq(schema::note_scripts::script_root.nullable())),
        ),
        (
            NoteRecordRaw::as_select(),
            schema::note_scripts::script.nullable(),
            rowid_sel.clone(),
        ),
    )
    .filter(schema::notes::execution_mode.eq(0_i32))
    .filter(schema::notes::consumed_block_num.is_null())
    .filter(rowid_sel_ge)
    .order(rowid_sel.asc())
    .limit(page.size.get() as i64 + 1)
    .load::<RawLoadedTuple>(conn)?;

    let mut notes = Vec::with_capacity(page.size.into());
    for raw_item in raw {
        let (raw_item, row_id) = split_into_raw_note_record_and_implicit_row_id(raw_item);
        page.token = None;
        if notes.len() == page.size.get() {
            page.token = Some(row_id as u64);
            break;
        }
        notes.push(TryInto::<NoteRecord>::try_into(raw_item)?);
    }

    Ok((notes, page))
}

/// Returns a paginated batch of network notes for an account that are unconsumed by a specified
/// block number.
///
/// Notes that have been consumed after the specified block number are included in the result.
///
/// # Returns
///
/// A set of unconsumed network notes with maximum length of `size` and the page to get
/// the next set.
//
// Attention: uses the _implicit_ column `rowid`, which requires to use a few raw SQL nugget
// statements
#[allow(
    clippy::cast_sign_loss,
    reason = "We need custom SQL statements which has given types that we need to convert"
)]
#[allow(
    clippy::too_many_lines,
    reason = "Lines will be reduced when schema is updated to simplify logic"
)]
pub(crate) fn select_unconsumed_network_notes_by_tag(
    conn: &mut SqliteConnection,
    tag: u32,
    latest_block_num: BlockNumber,
    mut page: Page,
) -> Result<(Vec<NoteRecord>, Page), DatabaseError> {
    assert_eq!(
        NoteExecutionMode::Network as u8,
        0,
        "Hardcoded execution value must match query"
    );

    let rowid_sel = diesel::dsl::sql::<diesel::sql_types::BigInt>("notes.rowid");
    let rowid_sel_ge =
        diesel::dsl::sql::<diesel::sql_types::Bool>("notes.rowid >= ")
            .bind::<diesel::sql_types::BigInt, i64>(page.token.unwrap_or_default() as i64);

    // SELECT {}, rowid
    // FROM notes
    // LEFT JOIN note_scripts ON notes.script_root = note_scripts.script_root
    // WHERE
    //  execution_mode = 0 AND tag = ?1 AND
    //  block_num <= ?2 AND
    //  (consumed_block_num IS NULL OR consumed_block_num > ?2) AND rowid >= ?3
    // ORDER BY rowid
    // LIMIT ?
    #[allow(
        clippy::items_after_statements,
        reason = "It's only relevant for a single call function"
    )]
    type RawLoadedTuple = (
        NoteRecordRaw,
        Option<Vec<u8>>, // script
        i64,             // rowid (from sql::<BigInt>("notes.rowid"))
    );

    #[allow(
        clippy::items_after_statements,
        reason = "It's only relevant for a single call function"
    )]
    fn split_into_raw_note_record_and_implicit_row_id(
        tuple: RawLoadedTuple,
    ) -> (NoteRecordWithScriptRaw, i64) {
        let (note, script, row) = tuple;
        let combined = NoteRecordWithScriptRaw::from((note, script));
        (combined, row)
    }

    let raw = SelectDsl::select(
        schema::notes::table.left_join(
            schema::note_scripts::table
                .on(schema::notes::script_root.eq(schema::note_scripts::script_root.nullable())),
        ),
        (
            NoteRecordRaw::as_select(),
            schema::note_scripts::script.nullable(),
            rowid_sel.clone(),
        ),
    )
    .filter(schema::notes::execution_mode.eq(0_i32))
    .filter(schema::notes::tag.eq(tag as i32))
    .filter(schema::notes::block_num.le(latest_block_num.to_raw_sql()))
    .filter(
        schema::notes::consumed_block_num
            .is_null()
            .or(schema::notes::consumed_block_num.gt(latest_block_num.to_raw_sql())),
    )
    .filter(rowid_sel_ge)
    .order(rowid_sel.asc())
    .limit(page.size.get() as i64 + 1)
    .load::<RawLoadedTuple>(conn)?;

    let mut notes = Vec::with_capacity(page.size.into());
    for raw_item in raw {
        let (raw_item, row_id) = split_into_raw_note_record_and_implicit_row_id(raw_item);
        page.token = None;
        if notes.len() == page.size.get() {
            page.token = Some(row_id as u64);
            break;
        }
        notes.push(TryInto::<NoteRecord>::try_into(raw_item)?);
    }

    Ok((notes, page))
}

/// Select all accounts from the DB using the given [Connection].
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
    let accounts_raw =
        QueryDsl::select(schema::accounts::table, models::AccountRaw::as_select()).load(conn)?;
    vec_raw_try_into(accounts_raw)
}

pub(crate) fn select_accounts_by_id(
    conn: &mut SqliteConnection,
    account_ids: Vec<AccountId>,
) -> Result<Vec<AccountInfo>, DatabaseError> {
    QueryParamAccountIdLimit::check(account_ids.len())?;

    let account_ids = account_ids.iter().map(|account_id| account_id.to_bytes().clone());

    let accounts_raw = QueryDsl::filter(
        QueryDsl::select(schema::accounts::table, models::AccountRaw::as_select()),
        schema::accounts::account_id.eq_any(account_ids),
    )
    .load(conn)?;
    vec_raw_try_into(accounts_raw)
}

/// Selects and merges account deltas by account id and block range from the DB using the given
/// [Connection].
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
        vault_key: vault_key_asset,
        is_remove: action,
        ..
    } in non_fungible_asset_updates
    {
        let asset = NonFungibleAsset::read_from_bytes(&vault_key_asset)
            .map_err(|err| DatabaseError::DataCorrupted(err.to_string()))?;

        match action {
            0 => non_fungible_delta.add(asset)?,
            1 => non_fungible_delta.remove(asset)?,
            _ => {
                return Err(DatabaseError::DataCorrupted(format!(
                    "Invalid non-fungible asset delta action: {action}"
                )));
            },
        }
    }

    let storage = AccountStorageDelta::from_parts(storage_scalars, storage_maps)?;
    let vault = AccountVaultDelta::new(FungibleAssetDelta::new(fungible)?, non_fungible_delta);

    Ok(Some(AccountDelta::new(account_id, storage, vault, nonce)?))
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
    pub(crate) is_remove: i32,
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

/// Select all the given block headers from the DB using the given [Connection].
///
/// # Note
///
/// Only returns the block headers that are actually present.
///
/// # Returns
pub fn select_block_headers(
    conn: &mut SqliteConnection,
    blocks: impl Iterator<Item = BlockNumber> + Send,
) -> Result<Vec<BlockHeader>, DatabaseError> {
    // The iterators are all deterministic, so is the conjunction.
    // All calling sites do it equivalently, hence the below holds.
    // <https://doc.rust-lang.org/src/core/slice/iter/macros.rs.html#195>
    // <https://doc.rust-lang.org/src/core/option.rs.html#2273>
    // And the conjunction is truthful:
    // <https://doc.rust-lang.org/src/core/iter/adapters/chain.rs.html#184>
    QueryParamBlockLimit::check(blocks.size_hint().0)?;

    // SELECT block_header FROM block_headers WHERE block_num IN rarray(?1)
    let blocks = Vec::from_iter(blocks.map(SqlTypeConvert::to_raw_sql));
    let raw_block_headers =
        QueryDsl::select(schema::block_headers::table, models::BlockHeaderRaw::as_select())
            .filter(schema::block_headers::block_num.eq_any(blocks))
            .load::<models::BlockHeaderRaw>(conn)?;
    vec_raw_try_into(raw_block_headers)
}

/// Select all block headers from the DB using the given [Connection].
///
/// # Returns
///
/// A vector of [`BlockHeader`] or an error.
pub fn select_all_block_headers(
    conn: &mut SqliteConnection,
) -> Result<Vec<BlockHeader>, DatabaseError> {
    // SELECT block_header FROM block_headers ORDER BY block_num ASC
    let raw_block_headers =
        QueryDsl::select(schema::block_headers::table, models::BlockHeaderRaw::as_select())
            .order(schema::block_headers::block_num.asc())
            .load::<models::BlockHeaderRaw>(conn)?;
    vec_raw_try_into(raw_block_headers)
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

/// Loads the data necessary for a note sync.
pub(crate) fn get_note_sync(
    conn: &mut SqliteConnection,
    block_num: BlockNumber,
    note_tags: &[u32],
) -> Result<NoteSyncUpdate, NoteSyncError> {
    QueryParamNoteTagLimit::check(note_tags.len()).map_err(DatabaseError::from)?;

    let notes = select_notes_since_block_by_tag_and_sender(conn, block_num, &[], note_tags)?;

    let block_header =
        select_block_header_by_block_num(conn, notes.first().map(|note| note.block_num))?
            .ok_or(NoteSyncError::EmptyBlockHeadersTable)?;
    Ok(NoteSyncUpdate { notes, block_header })
}

/// Select [`AccountSummary`] from the DB using the given [Connection], given that the account
/// update was done between `(block_start, block_end]`.
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
