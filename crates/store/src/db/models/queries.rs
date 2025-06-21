use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
};

use diesel::{
    JoinOnDsl, NullableExpressionMethods, OptionalExtension, SqliteConnection, alias,
    query_dsl::methods::SelectDsl,
};
use miden_lib::utils::{Deserializable, Serializable};
use miden_node_proto::domain::account::{AccountInfo, AccountSummary, NetworkAccountPrefix};
use miden_objects::{
    Word,
    account::{
        Account, AccountDelta, AccountId, AccountStorageDelta, AccountVaultDelta,
        FungibleAssetDelta, NonFungibleAssetDelta, NonFungibleDeltaAction, StorageMapDelta,
        delta::AccountUpdateDetails,
    },
    asset::NonFungibleAsset,
    block::{BlockAccountUpdate, BlockHeader, BlockNoteIndex, BlockNumber},
    crypto::{hash::rpo::RpoDigest, merkle::MerklePath},
    note::{NoteExecutionMode, NoteId, NoteInclusionProof, Nullifier},
    transaction::OrderedTransactionHeaders,
};

use super::{
    super::models, BoolExpressionMethods, DatabaseError, ExpressionMethods, NoteSyncRecordRawRow,
    QueryDsl, RunQueryDsl, SelectableHelper,
};
use crate::{
    db::{
        NoteRecord, NoteSyncRecord, NullifierInfo, Page, StateSyncUpdate, TransactionSummary,
        models::*,
        models::{AccountRaw, AccountSummaryRaw, NoteRecordRaw, TransactionSummaryRaw},
        schema,
    },
    errors::StateSyncError,
};

pub(crate) fn select_notes_since_block_by_tag_and_sender(
    conn: &mut SqliteConnection,
    block_number: BlockNumber,
    account_ids: &[AccountId],
    note_tags: &[u32],
) -> Result<Vec<NoteSyncRecord>, DatabaseError> {
    let desired_note_tags =
        Vec::from_iter(note_tags.iter().map(|tag| i32::from_be_bytes(tag.to_be_bytes())));
    let desired_senders = serialize_vec(account_ids.iter());

    let desired_block_num = block_number.as_u32() as i64;

    // select notes since block by tag and sender
    let Some(desired_block_num): Option<i64> =
        SelectDsl::select(schema::notes::table, schema::notes::block_num)
            .filter(
                schema::notes::tag
                    .eq_any(&desired_note_tags[..])
                    .or(schema::notes::sender.eq_any(&desired_senders[..])),
            )
            .filter(schema::notes::block_num.gt(desired_block_num))
            .order_by(schema::notes::block_num.asc())
            .first(conn)
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
            .load::<NoteSyncRecordRawRow>(conn)
            .map_err(DatabaseError::from)?;

    vec_raw_try_into(notes)
}

pub(crate) fn select_block_header_by_block_num(
    conn: &mut SqliteConnection,
    maybe_block_number: Option<BlockNumber>,
) -> Result<Option<BlockHeader>, DatabaseError> {
    let sel =
        SelectDsl::select(schema::block_headers::table, models::BlockHeaderRawRow::as_select());
    let row = if let Some(block_number) = maybe_block_number {
        sel.find(i64::from(block_number.as_u32()))
            .first::<models::BlockHeaderRawRow>(conn)
            .optional()?
        // invariant: only one block exists with the given block header, so the length is
        // always zero or one
    } else {
        sel.order(schema::block_headers::block_header.desc()).first(conn).optional()?
    };
    row.map(std::convert::TryInto::try_into).transpose()
}

pub(crate) fn select_note_inclusion_proofs(
    conn: &mut SqliteConnection,
    note_ids: &BTreeSet<NoteId>,
) -> Result<BTreeMap<NoteId, NoteInclusionProof>, DatabaseError> {
    let noted_ids_serialized = serialize_vec(note_ids.iter());

    let raw_notes = SelectDsl::select(
        schema::notes::table,
        (
            schema::notes::block_num,
            schema::notes::note_id,
            schema::notes::batch_index,
            schema::notes::note_index,
            schema::notes::merkle_path,
        ),
    )
    .filter(schema::notes::note_id.eq_any(noted_ids_serialized))
    .order_by(schema::notes::block_num.asc())
    .load::<(i64, Vec<u8>, i32, i32, Vec<u8>)>(conn)?;

    Result::<BTreeMap<_, _>, _>::from_iter(raw_notes.iter().map(
        |(block_num, note_id, batch_index, note_index, merkle_path)| {
            let note_id = NoteId::read_from_bytes(&note_id[..])?;
            let block_num = BlockNumber::from(*block_num as u32);
            let node_index_in_block =
                BlockNoteIndex::new(*batch_index as usize, *note_index as usize)
                    .expect("batch and note index from DB should be valid")
                    .leaf_index_value();
            let merkle_path = MerklePath::read_from_bytes(&merkle_path[..])?;
            let proof = NoteInclusionProof::new(block_num, node_index_in_block, merkle_path)?;
            Ok((note_id, proof))
        },
    ))
}

pub(crate) fn insert_block_header(
    conn: &mut SqliteConnection,
    block_header: &BlockHeader,
) -> Result<usize, DatabaseError> {
    let count = diesel::insert_into(schema::block_headers::table)
        .values(&[(
            schema::block_headers::block_num.eq(block_header.block_num().as_u32() as i64),
            schema::block_headers::block_header.eq(block_header.to_bytes()),
        )])
        .execute(conn)?;
    Ok(count)
}

/// Deserializes account and applies account delta.
pub(crate) fn apply_delta(
    _account_id: AccountId, // TODO error handline shifted _outside_
    mut account: Account,
    delta: &AccountDelta,
    final_state_commitment: &RpoDigest,
) -> crate::db::Result<Account, DatabaseError> {
    // TODO former error handling
    // let account = value.as_blob_or_null()?;
    // let account = account.map(Account::read_from_bytes).transpose()?;

    // let Some(mut account) = account else {
    //     return Err(DatabaseError::AccountNotpub(crate)lic(account_id));
    // };

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
        conn2: &mut SqliteConnection,
        account_id: AccountId,
        block_num: BlockNumber,
        nonce: u32,
    ) -> Result<usize, DatabaseError> {
        let count = diesel::insert_into(schema::account_deltas::table)
            .values(&[(
                schema::account_deltas::account_id.eq(account_id.to_bytes()),
                schema::account_deltas::block_num.eq(block_num.as_u32() as i64),
                schema::account_deltas::nonce.eq(nonce as i32),
            )])
            .execute(conn2)?;
        Ok(count)
    };

    let insert_slot_update_stmt = |conn2: &mut SqliteConnection,
                                   account_id: AccountId,
                                   block_num: BlockNumber,
                                   slot: u8,
                                   value: Vec<u8>|
     -> Result<usize, DatabaseError> {
        let count = diesel::insert_into(schema::account_storage_slot_updates::table)
            .values(&[(
                schema::account_storage_slot_updates::account_id.eq(account_id.to_bytes()),
                schema::account_storage_slot_updates::block_num.eq(block_num.as_u32() as i64),
                schema::account_storage_slot_updates::slot.eq(slot as i32),
                schema::account_storage_slot_updates::value.eq(value),
            )])
            .execute(conn2)?;
        Ok(count)
    };

    let insert_storage_map_update_stmt = |conn2: &mut SqliteConnection,
                                          account_id: AccountId,
                                          block_num: BlockNumber,
                                          slot: u32,
                                          key: Vec<u8>,
                                          value: Vec<u8>|
     -> Result<usize, DatabaseError> {
        let count = diesel::insert_into(schema::account_storage_map_updates::table)
            .values(&[(
                schema::account_storage_map_updates::account_id.eq(account_id.to_bytes()),
                schema::account_storage_map_updates::block_num.eq(block_num.as_u32() as i64),
                schema::account_storage_map_updates::slot.eq(slot as i32),
                schema::account_storage_map_updates::key.eq(key),
                schema::account_storage_map_updates::value.eq(value),
            )])
            .execute(conn2)?;
        Ok(count)
    };

    fn insert_fungible_asset_delta_stmt(
        conn2: &mut SqliteConnection,
        account_id: AccountId,
        block_num: BlockNumber,
        faucet_id: Vec<u8>,
        delta: u32,
    ) -> Result<usize, DatabaseError> {
        let count = diesel::insert_into(schema::account_fungible_asset_deltas::table)
            .values(&[(
                schema::account_fungible_asset_deltas::account_id.eq(account_id.to_bytes()),
                schema::account_fungible_asset_deltas::block_num.eq(block_num.as_u32() as i64),
                schema::account_fungible_asset_deltas::faucet_id.eq(faucet_id),
                schema::account_fungible_asset_deltas::delta.eq(delta as i32),
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
                schema::account_non_fungible_asset_updates::block_num.eq(block_num.as_u32() as i64),
                schema::account_non_fungible_asset_updates::vault_key.eq(vault_key),
                schema::account_non_fungible_asset_updates::is_remove.eq(is_remove),
            )])
            .execute(conn2)?;
        Ok(count)
    };

    insert_acc_delta_stmt(
        conn,
        account_id,
        block_number,
        delta.nonce().map(|x| x.inner() as u32).unwrap_or_default(),
    )?;

    for (&slot, value) in delta.storage().values() {
        insert_slot_update_stmt(conn, account_id, block_number, slot, value.to_bytes())?;
    }

    for (&slot, map_delta) in delta.storage().maps() {
        for (key, value) in map_delta.leaves() {
            insert_storage_map_update_stmt(
                conn,
                account_id,
                block_number,
                slot as u32,
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
            delta as u32, // FIXME TODO, types don't align
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

// there are a bunch of closures with detailed type annotations, which lengthens the function
// TODO some _might_ be extractable, they _should_ be context independent
//
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
            Some(NetworkAccountPrefix::try_from(account_id)?.inner())
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

                let insert_delta = AccountDelta::from(account.clone());

                (Some(Cow::Borrowed(account)), Some(Cow::Owned(insert_delta)))
            },
            AccountUpdateDetails::Delta(delta) => {
                let mut rows = select_details_stmt(conn, account_id)?.into_iter();
                let Some(account) = rows.next() else {
                    return Err(DatabaseError::AccountNotFoundInDb(account_id));
                };

                let account =
                    apply_delta(account_id, account, delta, &update.final_state_commitment())?;

                (Some(Cow::Owned(account)), Some(Cow::Borrowed(delta)))
            },
        };

        let val = (
            schema::accounts::account_id.eq(account_id.to_bytes()),
            schema::accounts::network_account_id_prefix
                .eq(network_account_id_prefix.map(|prefix| prefix as i64)),
            schema::accounts::account_commitment.eq(update.final_state_commitment().to_bytes()),
            schema::accounts::block_num.eq(block_num.as_u32() as i64),
            schema::accounts::details.eq(full_account.as_ref().map(|account| account.to_bytes())),
        );
        let v = val.clone();
        let inserted = diesel::insert_into(schema::accounts::table)
                .values(&v)
                // TODO do the update on conflict
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

pub(crate) fn insert_notes(
    conn: &mut SqliteConnection,
    notes: &[(NoteRecord, Option<Nullifier>)],
) -> Result<usize, DatabaseError> {
    let count = diesel::insert_into(schema::notes::table)
        .values(Vec::from_iter(notes.iter().map(|(note, nullifier)| {
            (
                schema::notes::block_num.eq(note.block_num.as_u32() as i64),
                schema::notes::batch_index.eq(note.note_index.batch_idx() as i32),
                schema::notes::note_index.eq(note.note_index.note_idx_in_batch() as i32),
                schema::notes::note_id.eq(note.note_id.to_bytes()),
                schema::notes::note_type.eq(note.metadata.note_type() as u8 as i32),
                schema::notes::sender.eq(note.metadata.sender().to_bytes()),
                schema::notes::tag.eq(note.metadata.tag().inner() as i32),
                schema::notes::execution_mode.eq(note.metadata.tag().execution_mode() as u8 as i32),
                schema::notes::aux.eq(Into::<u64>::into(note.metadata.aux()) as i64),
                schema::notes::execution_hint
                    .eq(Into::<u64>::into(note.metadata.execution_hint()) as i64),
                schema::notes::merkle_path.eq(note.merkle_path.to_bytes()),
                schema::notes::consumed.eq(false as u8 as i32), // New notes are always unconsumed.
                schema::notes::nullifier.eq(nullifier.as_ref().map(Nullifier::to_bytes)), // Beware: `Option<T>` also implements `to_bytes`, but this is not what you want.
                schema::notes::assets.eq(note.details.as_ref().map(|d| d.assets().to_bytes())),
                schema::notes::inputs.eq(note.details.as_ref().map(|d| d.inputs().to_bytes())),
                schema::notes::script_root
                    .eq(note.details.as_ref().map(|d| d.script().root().to_bytes())),
                schema::notes::serial_num
                    .eq(note.details.as_ref().map(|d| d.serial_num().to_bytes())),
            )
        })))
        .execute(conn)?;
    Ok(count)
}

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
                schema::transactions::block_num.eq(block_num.as_u32() as i64),
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
    let cols = (
        schema::notes::block_num,
        schema::notes::batch_index,
        schema::notes::note_index,
        schema::notes::note_id,
        // metadata
        schema::notes::note_type,
        schema::notes::sender,
        schema::notes::tag,
        schema::notes::aux,
        schema::notes::execution_hint,
        // details
        schema::notes::assets,
        schema::notes::inputs,
        schema::notes::serial_num,
        // // merkle
        schema::notes::merkle_path,
        schema::note_scripts::script.nullable(),
    );

    let q = schema::notes::table
        .left_join(
            schema::note_scripts::table
                .on(schema::notes::script_root.eq(schema::note_scripts::script_root.nullable())),
        )
        .filter(schema::notes::note_id.eq_any(&note_ids));
    let raw: Vec<_> = SelectDsl::select(
        q, cols, // NoteRecordRaw::as_select()
    )
    .load::<NoteRecordRaw>(conn)?;
    let records = vec_raw_try_into::<NoteRecord, _>(raw)?;
    Ok(records)
}

#[cfg(test)]
pub(crate) fn select_all_notes(
    conn: &mut SqliteConnection,
) -> Result<Vec<NoteRecord>, DatabaseError> {
    let cols = (
        schema::notes::block_num,
        schema::notes::batch_index,
        schema::notes::note_index,
        schema::notes::note_id,
        // metadata
        schema::notes::note_type,
        schema::notes::sender,
        schema::notes::tag,
        schema::notes::aux,
        schema::notes::execution_hint,
        // details
        schema::notes::assets,
        schema::notes::inputs,
        schema::notes::serial_num,
        // // merkle
        schema::notes::merkle_path,
        schema::note_scripts::script.nullable(),
    );

    let q = schema::notes::table.left_join(
        schema::note_scripts::table
            .on(schema::notes::script_root.eq(schema::note_scripts::script_root.nullable())),
    );
    let raw: Vec<_> = SelectDsl::select(
        q, cols, // NoteRecordRaw::as_select()
    )
    .order(schema::notes::block_num.asc())
    .load::<NoteRecordRaw>(conn)?;
    let records = vec_raw_try_into::<NoteRecord, _>(raw)?;
    Ok(records)
}

pub(crate) fn select_all_nullifiers(
    conn: &mut SqliteConnection,
) -> Result<Vec<NullifierInfo>, DatabaseError> {
    let nullifiers_raw = schema::nullifiers::table.load::<models::NullifierRawRow>(conn)?;
    vec_raw_try_into(nullifiers_raw)
}

pub(crate) fn insert_nullifiers_for_block(
    conn: &mut SqliteConnection,
    nullifiers: &[Nullifier],
    block_num: BlockNumber,
) -> Result<usize, DatabaseError> {
    let serialized_nullifiers =
        Vec::<Vec<u8>>::from_iter(nullifiers.iter().map(Nullifier::to_bytes));

    let mut count = diesel::update(schema::notes::table)
        .filter(schema::notes::nullifier.eq_any(&serialized_nullifiers))
        .set(schema::notes::consumed.eq(true as u8 as i32))
        .execute(conn)?;

    count += diesel::insert_into(schema::nullifiers::table)
        .values(Vec::from_iter(nullifiers.iter().zip(serialized_nullifiers.iter()).map(
            |(nullifier, bytes)| {
                (
                    schema::nullifiers::nullifier.eq(bytes),
                    schema::nullifiers::nullifier_prefix.eq(get_nullifier_prefix(nullifier) as i32),
                    schema::nullifiers::block_num.eq(block_num.as_u32() as i64),
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

pub(crate) fn select_nullifiers_by_prefix_q(
    conn: &mut SqliteConnection,
    _prefix_len: u8,
    nullifier_prefixes: &[u32],
    block_num: BlockNumber,
) -> Result<Vec<NullifierInfo>, DatabaseError> {
    // TODO XXX ensure these type conversions are sane
    let prefixes = nullifier_prefixes.iter().map(|prefix| i32::try_from(*prefix).unwrap());
    let nullifiers_raw =
        SelectDsl::select(schema::nullifiers::table, models::NullifierRawRow::as_select())
            .filter(schema::nullifiers::nullifier_prefix.eq_any(prefixes))
            .filter(schema::nullifiers::block_num.ge(i64::from(block_num.as_u32())))
            .order(schema::nullifiers::block_num.asc())
            .load::<models::NullifierRawRow>(conn)?;
    vec_raw_try_into(nullifiers_raw)
}

pub(crate) fn get_account_by_id_prefix(
    conn: &mut SqliteConnection,
    id_prefix: u32,
) -> Result<Option<AccountInfo>, DatabaseError> {
    let maybe_info = QueryDsl::filter(
        QueryDsl::select(schema::accounts::table, AccountRaw::as_select()),
        schema::accounts::network_account_id_prefix.eq(Some(i64::from(id_prefix))),
    )
    .first::<AccountRaw>(conn)
    .optional()
    .map_err(DatabaseError::Diesel)?;

    let result: Result<Option<AccountInfo>, DatabaseError> =
        maybe_info.map(std::convert::TryInto::<AccountInfo>::try_into).transpose();

    result
}

pub(crate) fn read_all_account_commitments(
    conn: &mut SqliteConnection,
) -> Result<Vec<(AccountId, RpoDigest)>, DatabaseError> {
    let raw = SelectDsl::select(
        schema::accounts::table,
        (schema::accounts::account_id, schema::accounts::account_commitment),
    )
    .order_by(schema::accounts::block_num.asc())
    .load::<(Vec<u8>, Vec<u8>)>(conn)?;

    Result::<Vec<_>, DatabaseError>::from_iter(raw.into_iter().map(
        |(ref account, ref commitment)| {
            Ok((AccountId::read_from_bytes(account)?, RpoDigest::read_from_bytes(commitment)?))
        },
    ))
}

pub(crate) fn get_account_details(
    conn: &mut SqliteConnection,
    id: AccountId,
) -> Result<AccountInfo, DatabaseError> {
    let val = QueryDsl::select(schema::accounts::table, models::AccountRaw::as_select())
        .find(id.to_bytes())
        .first::<models::AccountRaw>(conn)?;
    val.try_into()
}

// Attention: uses the _implicit_ column `rowid`, which requires to use a few raw SQL nugget statements
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

    type RawLoadedTuple = (
        i64,             // block_num
        i32,             // batch_index
        i32,             // note_index
        Vec<u8>,         // note_id
        i32,             // note_type
        Vec<u8>,         // sender
        i32,             // tag
        i64,             // aux
        i64,             // execution_hint
        Vec<u8>,         // merkle_path
        Option<Vec<u8>>, // assets
        Option<Vec<u8>>, // inputs
        Option<Vec<u8>>, // script
        Option<Vec<u8>>, // serial_num
        i64,             // rowid (from sql::<BigInt>("notes.rowid"))
    );

    fn split_into_raw_note_record_and_implicit_row_id(
        tuple: RawLoadedTuple,
    ) -> (NoteRecordRaw, i64) {
        (
            NoteRecordRaw {
                block_num: tuple.0,
                batch_index: tuple.1,
                note_index: tuple.2,
                note_id: tuple.3,
                note_type: tuple.4,
                sender: tuple.5,
                tag: tuple.6,
                aux: tuple.7,
                execution_hint: tuple.8,
                merkle_path: tuple.9,
                assets: tuple.10,
                inputs: tuple.11,
                script: tuple.12,
                serial_num: tuple.13,
            },
            tuple.14,
        )
    }

    let raw = SelectDsl::select(
        schema::notes::table.left_join(
            schema::note_scripts::table
                .on(schema::notes::script_root.eq(schema::note_scripts::script_root.nullable())),
        ),
        (
            schema::notes::block_num,
            schema::notes::batch_index,
            schema::notes::note_index,
            schema::notes::note_id,
            // metadata
            schema::notes::note_type,
            schema::notes::sender,
            schema::notes::tag,
            schema::notes::aux,
            schema::notes::execution_hint,
            schema::notes::merkle_path,
            // details
            schema::notes::assets,
            schema::notes::inputs,
            schema::note_scripts::script.nullable(),
            schema::notes::serial_num,
            rowid_sel.clone(),
        ),
    )
    .filter(schema::notes::execution_mode.eq(0_i32))
    .filter(schema::notes::consumed.eq(false as u8 as i32))
    .filter(rowid_sel_ge)
    .order(rowid_sel.asc())
    .limit(page.size.get() as i64 + 1)
    .load::<RawLoadedTuple>(conn)?;

    let mut notes = Vec::with_capacity(page.size.into());
    for raw_item in raw {
        let (raw_item, row) = split_into_raw_note_record_and_implicit_row_id(raw_item);
        page.token = None;
        if notes.len() == page.size.get() {
            page.token = Some(row as u64);
            break;
        }
        notes.push(TryInto::<NoteRecord>::try_into(raw_item)?);
    }

    Ok((notes, page))
}

#[cfg(test)]
pub(crate) fn select_all_accounts(
    conn: &mut SqliteConnection,
) -> Result<Vec<AccountInfo>, DatabaseError> {
    let accounts_raw =
        QueryDsl::select(schema::accounts::table, models::AccountRaw::as_select()).load(conn)?;
    vec_raw_try_into(accounts_raw)
}

pub(crate) fn select_accounts_by_id(
    conn: &mut SqliteConnection,
    account_ids: Vec<AccountId>,
) -> Result<Vec<AccountInfo>, DatabaseError> {
    let account_ids = account_ids.iter().map(|account_id| account_id.to_bytes().clone());

    let accounts_raw = QueryDsl::filter(
        QueryDsl::select(schema::accounts::table, models::AccountRaw::as_select()),
        schema::accounts::account_id.eq_any(account_ids),
    )
    .load(conn)?;
    vec_raw_try_into(accounts_raw)
}

pub(crate) fn select_account_delta(
    conn: &mut SqliteConnection,
    account_id: AccountId,
    block_start: BlockNumber,
    block_end: BlockNumber,
) -> Result<Option<AccountDelta>, DatabaseError> {
    let slot_updates = select_slot_updates_stmt(conn, &account_id, &block_start, &block_end)?;
    let storage_map_updates =
        select_storage_map_updates_stmt(conn, &account_id, &block_start, &block_end)?;
    let fungible_asset_deltas =
        select_fungible_asset_deltas_stmt(conn, &account_id, &block_start, &block_end)?;
    let non_fungible_asset_updates =
        select_non_fungible_asset_updates_stmt(conn, &account_id, &block_start, &block_end)?;

    let Some(nonce) = select_nonce_stmt(conn, &account_id, &block_start, &block_end)? else {
        return Ok(None);
    };
    let nonce = nonce.try_into().map_err(DatabaseError::InvalidFelt)?;

    let storage_scalars =
        Result::<BTreeMap<u8, Word>, _>::from_iter(slot_updates.iter().map(|(slot, value)| {
            let felt = Word::read_from_bytes(value)?;
            Ok::<_, DatabaseError>((*slot as u8, felt))
        }))?;

    let mut storage_maps = BTreeMap::<u8, StorageMapDelta>::new();
    for (slot, key, value) in storage_map_updates {
        let key = miden_objects::Digest::read_from_bytes(&key)?;
        let value = Word::read_from_bytes(&value)?;
        storage_maps.entry(slot as u8).or_default().insert(key, value);
    }

    let mut fungible = BTreeMap::<AccountId, i64>::new();
    for (faucet_id, value) in fungible_asset_deltas {
        let faucet_id = AccountId::read_from_bytes(&faucet_id)?;
        fungible.insert(faucet_id, value.unwrap()); // FIXME XXX deal with failability
    }

    let mut non_fungible_delta = NonFungibleAssetDelta::default();
    for (_, vault_key_asset, action) in non_fungible_asset_updates {
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

    let storage = AccountStorageDelta::new(storage_scalars, storage_maps)?;
    let vault = AccountVaultDelta::new(FungibleAssetDelta::new(fungible)?, non_fungible_delta);

    Ok(Some(AccountDelta::new(storage, vault, Some(nonce))?))
}

pub(crate) fn select_nonce_stmt(
    conn: &mut SqliteConnection,
    account_id: &AccountId,
    start_block_num: &BlockNumber,
    end_block_num: &BlockNumber,
) -> Result<Option<u64>, DatabaseError> {
    let desired_account_id = account_id.to_bytes();
    let start_block_num = start_block_num.as_u32() as i64;
    let end_block_num = end_block_num.as_u32() as i64;

    let res = SelectDsl::select(schema::account_deltas::table, schema::account_deltas::nonce)
        .filter(
            schema::account_deltas::account_id
                .eq(desired_account_id)
                .and(schema::account_deltas::block_num.gt(start_block_num))
                .and(schema::account_deltas::block_num.le(end_block_num)),
        )
        .order(schema::account_deltas::block_num.desc())
        .limit(1)
        .get_result::<i32>(conn)
        .optional()?;
    Ok(res.map(|nonce| nonce as u64))
}

// Attention: A more complex query, utilizing aliases for nested queries
pub(crate) fn select_slot_updates_stmt(
    conn: &mut SqliteConnection,
    account_id_val: &AccountId,
    start_block_num: &BlockNumber,
    end_block_num: &BlockNumber,
) -> Result<Vec<(i32, Vec<u8>)>, DatabaseError> {
    let desired_account_id = account_id_val.to_bytes();
    let start_block_num = start_block_num.as_u32() as i64;
    let end_block_num = end_block_num.as_u32() as i64;

    use schema::account_storage_slot_updates::dsl::{account_id, block_num, slot, value};

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

pub(crate) fn select_storage_map_updates_stmt(
    conn: &mut SqliteConnection,
    account_id_val: &AccountId,
    start_block_num: &BlockNumber,
    end_block_num: &BlockNumber,
) -> Result<Vec<(i32, Vec<u8>, Vec<u8>)>, DatabaseError> {
    use schema::account_storage_map_updates::dsl::{account_id, block_num, key, slot, value};

    let desired_account_id = account_id_val.to_bytes();
    let start_block_num = start_block_num.as_u32() as i64;
    let end_block_num = end_block_num.as_u32() as i64;

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
    let res: Vec<(i32, Vec<u8>, Vec<u8>)> = SelectDsl::select(a, (a.field(slot), a.field(key), a.field(value)))
        .filter(a.field(account_id).eq(&desired_account_id))
        .filter(a.field(block_num).gt(start_block_num))
        .filter(a.field(block_num).le(end_block_num))
        .filter(diesel::dsl::not(diesel::dsl::exists(subquery))) // Apply the NOT EXISTS condition
        .load(conn)?;
    Ok(res)
}

pub(crate) fn select_fungible_asset_deltas_stmt(
    conn: &mut SqliteConnection,
    account_id: &AccountId,
    start_block_num: &BlockNumber,
    end_block_num: &BlockNumber,
) -> Result<Vec<(Vec<u8>, Option<i64>)>, DatabaseError> {
    let desired_account_id = account_id.to_bytes();
    let start_block_num = start_block_num.as_u32() as i64;
    let end_block_num = end_block_num.as_u32() as i64;

    Ok(SelectDsl::select(
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
            diesel::dsl::sum(schema::account_fungible_asset_deltas::delta),
        ),
    )
    .load(conn)?)
}

pub(crate) fn select_non_fungible_asset_updates_stmt(
    conn: &mut SqliteConnection,
    account_id: &AccountId,
    start_block_num: &BlockNumber,
    end_block_num: &BlockNumber,
) -> Result<Vec<(i64, Vec<u8>, i32)>, DatabaseError> {
    let desired_account_id = account_id.to_bytes();
    let start_block_num = start_block_num.as_u32() as i64;
    let end_block_num = end_block_num.as_u32() as i64;

    Ok(SelectDsl::select(
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
    .get_results(conn)?)
}

pub fn select_all_block_headers(
    conn: &mut SqliteConnection,
) -> Result<Vec<BlockHeader>, DatabaseError> {
    let raw_block_headers =
        QueryDsl::select(schema::block_headers::table, models::BlockHeaderRawRow::as_select())
            .load::<models::BlockHeaderRawRow>(conn)?;
    vec_raw_try_into(raw_block_headers)
}

pub(crate) fn get_state_sync(
    conn: &mut SqliteConnection,
    block_number: BlockNumber,
    account_ids: Vec<AccountId>,
    note_tags: Vec<u32>,
) -> Result<StateSyncUpdate, StateSyncError> {
    let desired_senders = serialize_vec(account_ids.iter());
    let desired_note_tags =
        Vec::from_iter(note_tags.iter().map(|tag| i32::from_be_bytes(tag.to_be_bytes())));

    // select notes since block by tag and sender
    let desired_block_num = get_desired_block_num(conn, &desired_note_tags, &desired_senders)?;

    let notes = load_notes_by_tag_and_sender(
        conn,
        desired_block_num,
        &desired_note_tags,
        &desired_senders,
    )?;

    // select block header by block num
    let maybe_note_block_num = notes.first().map(|note| note.block_num);
    let block_header: BlockHeader = get_block_header_by_block_num(conn, maybe_note_block_num)?
        .ok_or_else(|| StateSyncError::EmptyBlockHeadersTable)?;

    // select accounts by block range
    let block_start: i64 = block_number.as_u32().into();
    let block_end: i64 = block_header.block_num().as_u32().into();
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
        notes: vec_raw_try_into(notes)?,
        block_header,
        account_updates,
        transactions,
    })
}

fn get_desired_block_num(
    conn: &mut SqliteConnection,
    desired_note_tags: &[i32],
    desired_senders: &[Vec<u8>],
) -> Result<i64, StateSyncError> {
    SelectDsl::select(schema::notes::table, schema::notes::block_num)
        .filter(
            schema::notes::tag
                .eq_any(desired_note_tags)
                .or(schema::notes::sender.eq_any(desired_senders)),
        )
        .order_by(schema::notes::block_num.asc())
        .limit(1)
        .get_result(conn)
        .optional()
        .map_err(DatabaseError::from)?
        .ok_or_else(|| StateSyncError::EmptyBlockHeadersTable)
}

fn load_notes_by_tag_and_sender(
    conn: &mut SqliteConnection,
    desired_block_num: i64,
    desired_note_tags: &[i32],
    desired_senders: &[Vec<u8>],
) -> Result<Vec<NoteSyncRecordRawRow>, StateSyncError> {
    Ok(SelectDsl::select(schema::notes::table, NoteSyncRecordRawRow::as_select())
        .filter(schema::notes::block_num.eq(desired_block_num))
        .filter(
            schema::notes::tag
                .eq_any(desired_note_tags)
                .or(schema::notes::sender.eq_any(desired_senders)),
        )
        .load::<NoteSyncRecordRawRow>(conn)
        .map_err(DatabaseError::from)?)
}

fn get_block_header_by_block_num(
    conn: &mut SqliteConnection,
    maybe_note_block_num: Option<i64>,
) -> Result<Option<BlockHeader>, DatabaseError> {
    let sel =
        SelectDsl::select(schema::block_headers::table, models::BlockHeaderRawRow::as_select());
    let row = if let Some(block_number) = maybe_note_block_num {
        sel.find(block_number).first::<models::BlockHeaderRawRow>(conn).optional()
    } else {
        sel.order(schema::block_headers::block_header.desc()).first(conn).optional()
    }
    .map_err(DatabaseError::from)?;
    row.map(std::convert::TryInto::try_into).transpose()
}

pub fn select_accounts_by_block_range(
    conn: &mut SqliteConnection,
    account_ids: &[AccountId],
    block_start: i64,
    block_end: i64,
) -> Result<Vec<AccountSummary>, DatabaseError> {
    let desired_account_ids = serialize_vec(account_ids);
    let raw: Vec<AccountSummaryRaw> =
        SelectDsl::select(schema::accounts::table, AccountSummaryRaw::as_select())
            .filter(schema::accounts::block_num.gt(block_start))
            .filter(schema::accounts::block_num.le(block_end))
            .filter(schema::accounts::account_id.eq_any(desired_account_ids))
            .order(schema::accounts::block_num.asc())
            .load::<AccountSummaryRaw>(conn)?;
    Ok(vec_raw_try_into(raw).unwrap())
}

pub fn select_transactions_by_accounts_and_block_range(
    conn: &mut SqliteConnection,
    account_ids: &[AccountId],
    block_start: i64,
    block_end: i64,
) -> Result<Vec<TransactionSummary>, DatabaseError> {
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
