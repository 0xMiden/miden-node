//! Wrapper functions for SQL statements.

#[macro_use]
pub(crate) mod utils;

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    num::NonZeroUsize,
    rc::Rc,
};

use miden_node_proto::domain::account::{AccountInfo, AccountSummary, NetworkAccountPrefix};
use miden_node_utils::{
    ErrorReport,
    limiter::{
        QueryParamAccountIdLimit, QueryParamBlockLimit, QueryParamLimiter, QueryParamNoteIdLimit,
        QueryParamNoteTagLimit, QueryParamNullifierLimit, QueryParamNullifierPrefixLimit,
    },
};
use miden_objects::{
    LexicographicWord, Word,
    account::{
        Account, AccountDelta, AccountId, AccountStorageDelta, AccountVaultDelta,
        FungibleAssetDelta, NonFungibleAssetDelta, NonFungibleDeltaAction, StorageMapDelta,
        StorageSlot, delta::AccountUpdateDetails,
    },
    asset::{Asset, NonFungibleAsset},
    block::{BlockAccountUpdate, BlockHeader, BlockNoteIndex, BlockNumber},
    crypto::merkle::SparseMerklePath,
    note::{NoteExecutionMode, NoteId, NoteInclusionProof, NoteMetadata, NoteType, Nullifier},
    transaction::{OrderedTransactionHeaders, TransactionId},
    utils::serde::{Deserializable, Serializable},
};
use rusqlite::{params, types::Value};
use utils::{read_block_number, read_from_blob_column};

use super::{
    NoteRecord, NoteSyncRecord, NoteSyncUpdate, NullifierInfo, Result, StateSyncUpdate,
    TransactionSummary,
};
use crate::{
    db::{
        sql::utils::{
            account_info_from_row, account_summary_from_row, apply_delta, column_value_as_u64,
            get_nullifier_prefix, u64_to_value,
        },
        transaction::Transaction,
    },
    errors::{DatabaseError, NoteSyncError, StateSyncError},
};

/// The page token and size to query from the DB.
#[derive(Debug, Copy, Clone)]
pub struct Page {
    pub token: Option<u64>,
    pub size: NonZeroUsize,
}

// ACCOUNT QUERIES
// ================================================================================================

/// Select all accounts from the DB using the given [Connection].
///
/// # Returns
///
/// A vector with accounts, or an error.
#[cfg(test)]
pub fn select_all_accounts(transaction: &Transaction) -> Result<Vec<AccountInfo>> {
    let mut stmt = transaction.prepare_cached(
        "
        SELECT
            account_id,
            account_commitment,
            block_num,
            details
        FROM
            accounts
        ORDER BY
            block_num ASC;
    ",
    )?;
    let mut rows = stmt.query([])?;

    let mut accounts = vec![];
    while let Some(row) = rows.next()? {
        accounts.push(account_info_from_row(row)?);
    }
    Ok(accounts)
}

/// Select all account commitments from the DB using the given [Connection].
///
/// # Returns
///
/// The vector with the account id and corresponding commitment, or an error.
pub fn select_all_account_commitments(transaction: &Transaction) -> Result<Vec<(AccountId, Word)>> {
    let mut stmt = transaction.prepare_cached(
        "SELECT account_id, account_commitment FROM accounts ORDER BY block_num ASC;",
    )?;
    let mut rows = stmt.query([])?;

    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        let account_id: AccountId = read_from_blob_column(row, 0)?;
        let account_commitment_data = row.get_ref(1)?.as_blob()?;
        let account_commitment = Word::read_from_bytes(account_commitment_data)?;

        result.push((account_id, account_commitment));
    }

    Ok(result)
}

/// Select [`AccountSummary`] from the DB using the given [Connection], given that the account
/// update was done between `(block_start, block_end]`.
///
/// # Returns
///
/// The vector of [`AccountSummary`] with the matching accounts.
pub fn select_accounts_by_block_range(
    transaction: &Transaction,
    block_start: BlockNumber,
    block_end: BlockNumber,
    account_ids: &[AccountId],
) -> Result<Vec<AccountSummary>> {
    QueryParamAccountIdLimit::check(account_ids.len())?;

    let mut stmt = transaction.prepare_cached(
        "
        SELECT
            account_id,
            account_commitment,
            block_num
        FROM
            accounts
        WHERE
            block_num > ?1 AND
            block_num <= ?2 AND
            account_id IN rarray(?3)
        ORDER BY
            block_num ASC
    ",
    )?;

    let account_ids: Vec<Value> = account_ids
        .iter()
        .copied()
        .map(|account_id| account_id.to_bytes().into())
        .collect();
    let mut rows =
        stmt.query(params![block_start.as_u32(), block_end.as_u32(), Rc::new(account_ids)])?;

    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        result.push(account_summary_from_row(row)?);
    }

    Ok(result)
}

/// Select the latest account details by account id from the DB using the given [Connection].
///
/// # Returns
///
/// The latest account details, or an error.
pub fn select_account(transaction: &Transaction, account_id: AccountId) -> Result<AccountInfo> {
    let mut stmt = transaction.prepare_cached(
        "
        SELECT
            account_id,
            account_commitment,
            block_num,
            details
        FROM
            accounts
        WHERE
            account_id = ?1;
    ",
    )?;

    let mut rows = stmt.query(params![account_id.to_bytes()])?;
    let row = rows.next()?.ok_or(DatabaseError::AccountNotFoundInDb(account_id))?;

    account_info_from_row(row)
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
pub fn select_network_account_by_prefix(
    transaction: &Transaction,
    id_prefix: u32,
) -> Result<Option<AccountInfo>> {select_accounts_
    let mut stmt = transaction.prepare_cached(
        "
        SELECT
            account_id,
            account_commitment,
            block_num,
            details
        FROM
            accounts
        WHERE
            network_account_id_prefix = ?1;
        ",
    )?;

    let mut rows = stmt.query(params![i64::from(id_prefix)])?;
    let row = rows.next()?;
    if let Some(row) = row {
        Ok(Some(account_info_from_row(row)?))
    } else {
        Ok(None)
    }
}

<<<<<<< HEAD
/// Select the latest accounts' details filtered by IDs from the DB using the given
/// [Connection].
///
/// # Returns
///
/// The account details vector, or an error.
pub fn select_accounts_by_ids(
    transaction: &Transaction,
    account_ids: &[AccountId],
) -> Result<Vec<AccountInfo>> {
    QueryParamAccountIdLimit::check(account_ids.len())?;

    let mut stmt = transaction.prepare_cached(
        "
        SELECT
            account_id,
            account_commitment,
            block_num,
            details
        FROM
            accounts
        WHERE
            account_id IN rarray(?1);
    ",
    )?;

    let account_ids: Vec<Value> = account_ids
        .iter()
        .copied()
        .map(|account_id| account_id.to_bytes().into())
        .collect();
    let mut rows = stmt.query(params![Rc::new(account_ids)])?;

    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        result.push(account_info_from_row(row)?);
    }

    Ok(result)
}

||||||| parent of 3a1bb2f (refactor: migrate to diesel-rs)
/// Select the latest accounts' details filtered by IDs from the DB using the given
/// [Connection].
///
/// # Returns
///
/// The account details vector, or an error.
pub fn select_accounts_by_ids(
    transaction: &Transaction,
    account_ids: &[AccountId],
) -> Result<Vec<AccountInfo>> {
    let mut stmt = transaction.prepare_cached(
        "
        SELECT
            account_id,
            account_commitment,
            block_num,
            details
        FROM
            accounts
        WHERE
            account_id IN rarray(?1);
    ",
    )?;

    let account_ids: Vec<Value> = account_ids
        .iter()
        .copied()
        .map(|account_id| account_id.to_bytes().into())
        .collect();
    let mut rows = stmt.query(params![Rc::new(account_ids)])?;

    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        result.push(account_info_from_row(row)?);
    }

    Ok(result)
}

=======
>>>>>>> 3a1bb2f (refactor: migrate to diesel-rs)
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
#[allow(clippy::too_many_lines, reason = "mostly just formatted sql text")]
pub fn select_account_delta(
    transaction: &Transaction,
    account_id: AccountId,
    block_start: BlockNumber,
    block_end: BlockNumber,
) -> Result<Option<AccountDelta>> {
    let mut select_nonce_stmt = transaction.prepare_cached(
        "
        SELECT
            SUM(nonce)
        FROM
            account_deltas
        WHERE
            account_id = ?1 AND block_num > ?2 AND block_num <= ?3
    ",
    )?;

    let mut select_slot_updates_stmt = transaction.prepare_cached(
        "
            SELECT
                slot, value
            FROM
                account_storage_slot_updates AS a
            WHERE
                account_id = ?1 AND
                block_num > ?2 AND
                block_num <= ?3 AND
                NOT EXISTS(
                    SELECT 1
                    FROM account_storage_slot_updates AS b
                    WHERE
                        b.account_id = ?1 AND
                        a.slot = b.slot AND
                        a.block_num < b.block_num AND
                        b.block_num <= ?3
                )
        ",
    )?;

    let mut select_storage_map_updates_stmt = transaction.prepare_cached(
        "
        SELECT
            slot, key, value
        FROM
            account_storage_map_updates AS a
        WHERE
            account_id = ?1 AND
            block_num > ?2 AND
            block_num <= ?3 AND
            NOT EXISTS(
                SELECT 1
                FROM account_storage_map_updates AS b
                WHERE
                    b.account_id = ?1 AND
                    a.slot = b.slot AND
                    a.key = b.key AND
                    a.block_num < b.block_num AND
                    b.block_num <= ?3
            )
        ",
    )?;

    let mut select_fungible_asset_deltas_stmt = transaction.prepare_cached(
        "
        SELECT
            faucet_id, SUM(delta)
        FROM
            account_fungible_asset_deltas
        WHERE
            account_id = ?1 AND
            block_num > ?2 AND
            block_num <= ?3
        GROUP BY
            faucet_id
        ",
    )?;

    let mut select_non_fungible_asset_updates_stmt = transaction.prepare_cached(
        "
        SELECT
            block_num, vault_key, is_remove
        FROM
            account_non_fungible_asset_updates
        WHERE
            account_id = ?1 AND
            block_num > ?2 AND
            block_num <= ?3
        ORDER BY
            block_num
        ",
    )?;

    let account_id_bytes = account_id.to_bytes();
    let nonce = match select_nonce_stmt
        .query_row(params![account_id_bytes, block_start.as_u32(), block_end.as_u32()], |row| {
            row.get::<_, Option<u64>>(0)
        }) {
        Ok(Some(nonce)) => nonce.try_into().map_err(DatabaseError::InvalidFelt)?,
        Err(rusqlite::Error::QueryReturnedNoRows) | Ok(None) => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    let mut storage_scalars = BTreeMap::new();
    let mut rows = select_slot_updates_stmt.query(params![
        account_id_bytes,
        block_start.as_u32(),
        block_end.as_u32()
    ])?;
    while let Some(row) = rows.next()? {
        let slot = row.get(0)?;
        let value_data = row.get_ref(1)?.as_blob()?;
        let value = Word::read_from_bytes(value_data)?;
        storage_scalars.insert(slot, value);
    }

    let mut storage_maps = BTreeMap::new();
    let mut rows = select_storage_map_updates_stmt.query(params![
        account_id_bytes,
        block_start.as_u32(),
        block_end.as_u32()
    ])?;
    while let Some(row) = rows.next()? {
        let slot = row.get(0)?;
        let key_data = row.get_ref(1)?.as_blob()?;
        let key = Word::read_from_bytes(key_data)?;
        let value_data = row.get_ref(2)?.as_blob()?;
        let value = Word::read_from_bytes(value_data)?;

        match storage_maps.entry(slot) {
            Entry::Vacant(entry) => {
                let key = LexicographicWord::new(key);
                entry.insert(StorageMapDelta::new(BTreeMap::from([(key, value)])));
            },
            Entry::Occupied(mut entry) => {
                entry.get_mut().insert(key, value);
            },
        }
    }

    let mut fungible = BTreeMap::new();
    let mut rows = select_fungible_asset_deltas_stmt.query(params![
        account_id_bytes,
        block_start.as_u32(),
        block_end.as_u32()
    ])?;
    while let Some(row) = rows.next()? {
        let faucet_id: AccountId = read_from_blob_column(row, 0)?;
        let value = row.get(1)?;
        fungible.insert(faucet_id, value);
    }

    let mut non_fungible_delta = NonFungibleAssetDelta::default();
    let mut rows = select_non_fungible_asset_updates_stmt.query(params![
        account_id_bytes,
        block_start.as_u32(),
        block_end.as_u32()
    ])?;
    while let Some(row) = rows.next()? {
        let vault_key_data = row.get_ref(1)?.as_blob()?;
        let asset = NonFungibleAsset::read_from_bytes(vault_key_data)
            .map_err(|err| DatabaseError::DataCorrupted(err.as_report()))?;
        let action: usize = row.get(2)?;
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

/// Inserts or updates accounts to the DB using the given [Transaction].
///
/// # Returns
///
/// The number of affected rows.
///
/// # Note
///
/// The [Transaction] object is not consumed. It's up to the caller to commit or rollback the
/// transaction.
pub fn upsert_accounts(
    transaction: &Transaction,
    accounts: &[BlockAccountUpdate],
    block_num: BlockNumber,
) -> Result<usize> {
    let mut upsert_stmt = transaction.prepare_cached(insert_sql!(
        accounts {
            account_id,
            network_account_id_prefix,
            account_commitment,
            block_num,
            details
        } | REPLACE
    ))?;

    let mut select_details_stmt =
        transaction.prepare_cached("SELECT details FROM accounts WHERE account_id = ?1")?;

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

                let insert_delta = build_insert_delta(account)?;

                (Some(Cow::Borrowed(account)), Some(Cow::Owned(insert_delta)))
            },
            AccountUpdateDetails::Delta(delta) => {
                let mut rows = select_details_stmt.query(params![account_id.to_bytes()])?;
                let Some(row) = rows.next()? else {
                    return Err(DatabaseError::AccountNotFoundInDb(account_id));
                };

                let account = apply_delta(
                    account_id,
                    &row.get_ref(0)?,
                    delta,
                    &update.final_state_commitment(),
                )?;

                (Some(Cow::Owned(account)), Some(Cow::Borrowed(delta)))
            },
        };

        let inserted = upsert_stmt.execute(params![
            account_id.to_bytes(),
            network_account_id_prefix,
            update.final_state_commitment().to_bytes(),
            block_num.as_u32(),
            full_account.as_ref().map(|account| account.to_bytes()),
        ])?;

        debug_assert_eq!(inserted, 1);

        if let Some(delta) = insert_delta {
            insert_account_delta(transaction, account_id, block_num, &delta)?;
        }

        count += inserted;
    }

    Ok(count)
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

/// Inserts account delta to the DB using the given [Transaction].
fn insert_account_delta(
    transaction: &Transaction,
    account_id: AccountId,
    block_number: BlockNumber,
    delta: &AccountDelta,
) -> Result<()> {
    let mut insert_acc_delta_stmt =
        transaction.prepare_cached(insert_sql!(account_deltas { account_id, block_num, nonce }))?;

    let mut insert_slot_update_stmt =
        transaction.prepare_cached(insert_sql!(account_storage_slot_updates {
            account_id,
            block_num,
            slot,
            value,
        }))?;

    let mut insert_storage_map_update_stmt =
        transaction.prepare_cached(insert_sql!(account_storage_map_updates {
            account_id,
            block_num,
            slot,
            key,
            value,
        }))?;

    let mut insert_fungible_asset_delta_stmt =
        transaction.prepare_cached(insert_sql!(account_fungible_asset_deltas {
            account_id,
            block_num,
            faucet_id,
            delta,
        }))?;

    let mut insert_non_fungible_asset_update_stmt =
        transaction.prepare_cached(insert_sql!(account_non_fungible_asset_updates {
            account_id,
            block_num,
            vault_key,
            is_remove,
        }))?;

    insert_acc_delta_stmt.execute(params![
        account_id.to_bytes(),
        block_number.as_u32(),
        delta.nonce_delta().as_int()
    ])?;

    for (&slot, value) in delta.storage().values() {
        insert_slot_update_stmt.execute(params![
            account_id.to_bytes(),
            block_number.as_u32(),
            slot,
            value.to_bytes()
        ])?;
    }

    for (&slot, map_delta) in delta.storage().maps() {
        for (key, value) in map_delta.entries() {
            insert_storage_map_update_stmt.execute(params![
                account_id.to_bytes(),
                block_number.as_u32(),
                slot,
                key.to_bytes(),
                value.to_bytes(),
            ])?;
        }
    }

    for (&faucet_id, &delta) in delta.vault().fungible().iter() {
        insert_fungible_asset_delta_stmt.execute(params![
            account_id.to_bytes(),
            block_number.as_u32(),
            faucet_id.to_bytes(),
            delta,
        ])?;
    }

    for (&asset, action) in delta.vault().non_fungible().iter() {
        let is_remove = match action {
            NonFungibleDeltaAction::Add => 0,
            NonFungibleDeltaAction::Remove => 1,
        };
        insert_non_fungible_asset_update_stmt.execute(params![
            account_id.to_bytes(),
            block_number.as_u32(),
            asset.to_bytes(),
            is_remove,
        ])?;
    }

    Ok(())
}

// NULLIFIER QUERIES
// ================================================================================================

/// Commit nullifiers to the DB using the given [Transaction]. This inserts the nullifiers into the
/// nullifiers table, and marks the note as consumed (if it was public).
///
/// # Returns
///
/// The number of affected rows.
///
/// # Note
///
/// The [Transaction] object is not consumed. It's up to the caller to commit or rollback the
/// transaction.
pub fn insert_nullifiers_for_block(
    transaction: &Transaction,
    nullifiers: &[Nullifier],
    block_num: BlockNumber,
) -> Result<usize> {
    QueryParamNullifierLimit::check(nullifiers.len())?;

    let serialized_nullifiers: Vec<Value> =
        nullifiers.iter().map(Nullifier::to_bytes).map(Into::into).collect();
    let serialized_nullifiers = Rc::new(serialized_nullifiers);

    let mut stmt = transaction
        .prepare_cached("UPDATE notes SET consumed = TRUE WHERE nullifier IN rarray(?1)")?;
    let mut count = stmt.execute(params![serialized_nullifiers])?;

    let mut stmt = transaction.prepare_cached(insert_sql!(nullifiers {
        nullifier,
        nullifier_prefix,
        block_num
    }))?;

    for (nullifier, bytes) in nullifiers.iter().zip(serialized_nullifiers.iter()) {
        count +=
            stmt.execute(params![bytes, get_nullifier_prefix(nullifier), block_num.as_u32()])?;
    }
    Ok(count)
}

/// Select all nullifiers from the DB using the given [Connection].
///
/// # Returns
///
/// A vector with nullifiers and the block height at which they were created, or an error.
pub fn select_all_nullifiers(transaction: &Transaction) -> Result<Vec<NullifierInfo>> {
    let mut stmt = transaction
        .prepare_cached("SELECT nullifier, block_num FROM nullifiers ORDER BY block_num ASC")?;
    let mut rows = stmt.query([])?;

    let mut result = vec![];
    while let Some(row) = rows.next()? {
        let nullifier_data = row.get_ref(0)?.as_blob()?;
        let nullifier = Nullifier::read_from_bytes(nullifier_data)?;
        let block_num = read_block_number(row, 1)?;
        result.push(NullifierInfo { nullifier, block_num });
    }
    Ok(result)
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
/// created, or an error.
pub fn select_nullifiers_by_prefix(
    transaction: &Transaction,
    prefix_len: u32,
    nullifier_prefixes: &[u32],
    block_num: BlockNumber,
) -> Result<Vec<NullifierInfo>> {
    assert_eq!(prefix_len, 16, "Only 16-bit prefixes are supported");

    QueryParamNullifierPrefixLimit::check(nullifier_prefixes.len())?;

    let nullifier_prefixes: Vec<Value> =
        nullifier_prefixes.iter().copied().map(Into::into).collect();

    let mut stmt = transaction.prepare_cached(
        "
        SELECT
            nullifier,
            block_num
        FROM
            nullifiers
        WHERE
            nullifier_prefix IN rarray(?1) AND
            block_num >= ?2
        ORDER BY
            block_num ASC
    ",
    )?;
    let mut rows = stmt.query(params![Rc::new(nullifier_prefixes), block_num.as_u32()])?;

    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        let nullifier_data = row.get_ref(0)?.as_blob()?;
        let nullifier = Nullifier::read_from_bytes(nullifier_data)?;
        let block_num = read_block_number(row, 1)?;
        result.push(NullifierInfo { nullifier, block_num });
    }
    Ok(result)
}

// NOTE QUERIES
// ================================================================================================

/// Select all notes from the DB using the given [Connection].
///
///
/// # Returns
///
/// A vector with notes, or an error.
pub fn select_all_notes(transaction: &Transaction) -> Result<Vec<NoteRecord>> {
    let mut stmt = transaction.prepare_cached(&format!(
        "SELECT {}
        FROM notes
        LEFT JOIN note_scripts ON notes.script_root = note_scripts.script_root
        ORDER BY block_num ASC",
        NoteRecord::SELECT_COLUMNS,
    ))?;
    let mut rows = stmt.query([])?;

    let mut notes = vec![];
    while let Some(row) = rows.next()? {
        notes.push(NoteRecord::from_row(row)?);
    }
    Ok(notes)
}

/// Insert notes to the DB using the given [Transaction]. Public notes should also have a nullifier.
///
/// # Returns
///
/// The number of affected rows.
///
/// # Note
///
/// The [Transaction] object is not consumed. It's up to the caller to commit or rollback the
/// transaction.
pub fn insert_notes(
    transaction: &Transaction,
    notes: &[(NoteRecord, Option<Nullifier>)],
) -> Result<usize> {
    let mut stmt = transaction.prepare_cached(insert_sql!(notes {
        block_num,
        batch_index,
        note_index,
        note_id,
        note_type,
        sender,
        tag,
        execution_mode,
        aux,
        execution_hint,
        inclusion_path,
        consumed,
        nullifier,
        assets,
        inputs,
        script_root,
        serial_num,
    }))?;

    let mut count = 0;
    for (note, nullifier) in notes {
        count += stmt.execute(params![
            note.block_num.as_u32(),
            note.note_index.batch_idx(),
            note.note_index.note_idx_in_batch(),
            note.note_id.to_bytes(),
            note.metadata.note_type() as u8,
            note.metadata.sender().to_bytes(),
            note.metadata.tag().as_u32(),
            note.metadata.tag().execution_mode() as u8,
            u64_to_value(note.metadata.aux().into()),
            u64_to_value(note.metadata.execution_hint().into()),
            note.inclusion_path.to_bytes(),
            // New notes are always unconsumed.
            false,
            // Beware: `Option<T>` also implements `to_bytes`, but this is not what you want.
            nullifier.as_ref().map(Nullifier::to_bytes),
            note.details.as_ref().map(|d| d.assets().to_bytes()),
            note.details.as_ref().map(|d| d.inputs().to_bytes()),
            note.details.as_ref().map(|d| d.script().root().to_bytes()),
            note.details.as_ref().map(|d| d.serial_num().to_bytes()),
        ])?;
    }

    Ok(count)
}

/// Insert scripts to the DB using the given [Transaction]. It inserts the scripts held by the
/// notes passed as parameter. If the script root already exists in the DB, it will be ignored.
///
/// # Returns
///
/// The number of affected rows.
///
/// # Note
///
/// The [Transaction] object is not consumed. It's up to the caller to commit or rollback the
/// transaction.
pub fn insert_scripts<'a>(
    transaction: &Transaction,
    notes: impl IntoIterator<Item = &'a NoteRecord>,
) -> Result<usize> {
    let mut stmt =
        transaction.prepare_cached(insert_sql!(note_scripts { script_root, script } | IGNORE))?;

    let mut count = 0;
    for note in notes {
        if let Some(ref details) = note.details {
            count += stmt.execute(params![
                details.script().root().to_bytes(),
                details.script().to_bytes(),
            ])?;
        }
    }

    Ok(count)
}

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
pub fn select_notes_since_block_by_tag_and_sender(
    transaction: &Transaction,
    tags: &[u32],
    account_ids: &[AccountId],
    block_num: BlockNumber,
) -> Result<Vec<NoteSyncRecord>> {
    QueryParamAccountIdLimit::check(account_ids.len())?;
    QueryParamNoteTagLimit::check(tags.len())?;

    let mut stmt = transaction
        .prepare_cached(include_str!("queries/select_notes_since_block_by_tag_and_sender.sql"))?;

    let tags: Vec<Value> = tags.iter().copied().map(Into::into).collect();
    let account_ids: Vec<Value> = account_ids
        .iter()
        .copied()
        .map(|account_id| account_id.to_bytes().into())
        .collect();
    let mut rows = stmt.query(params![Rc::new(tags), Rc::new(account_ids), block_num.as_u32()])?;

    let mut res = Vec::new();
    while let Some(row) = rows.next()? {
        let block_num = read_block_number(row, 0)?;
        let batch_idx = row.get(1)?;
        let note_idx_in_batch = row.get(2)?;
        // SAFETY: We can assume the batch and note indices stored in the DB are valid so this
        // should never panic.
        let note_index = BlockNoteIndex::new(batch_idx, note_idx_in_batch)
            .expect("batch and note index from DB should be valid");
        let note_id = read_from_blob_column(row, 3)?;
        let note_type = row.get::<_, u8>(4)?;
        let sender = read_from_blob_column(row, 5)?;
        let tag: u32 = row.get(6)?;
        let aux: u64 = row.get(7)?;
        let aux = aux.try_into().map_err(DatabaseError::InvalidFelt)?;
        let execution_hint = column_value_as_u64(row, 8)?;
        let inclusion_path = read_from_blob_column(row, 9)?;

        let metadata = NoteMetadata::new(
            sender,
            NoteType::try_from(note_type)?,
            tag.into(),
            execution_hint.try_into()?,
            aux,
        )?;

        let note = NoteSyncRecord {
            block_num,
            note_index,
            note_id,
            metadata,
            inclusion_path,
        };
        res.push(note);
    }
    Ok(res)
}

<<<<<<< HEAD
/// Select Note's matching the `NoteId` using the given [Connection].
///
/// # Returns
///
/// - Empty vector if no matching `note`.
/// - Otherwise, notes which `note_id` matches the `NoteId` as bytes.
pub fn select_notes_by_id(
    transaction: &Transaction,
    note_ids: &[NoteId],
) -> Result<Vec<NoteRecord>> {
    QueryParamNoteIdLimit::check(note_ids.len())?;

    let note_ids: Vec<Value> = note_ids.iter().map(|id| id.to_bytes().into()).collect();

    let mut stmt = transaction.prepare_cached(&format!(
        "SELECT {}
        FROM notes
        LEFT JOIN note_scripts ON notes.script_root = note_scripts.script_root
        WHERE note_id IN rarray(?1)",
        NoteRecord::SELECT_COLUMNS
    ))?;
    let mut rows = stmt.query(params![Rc::new(note_ids)])?;

    let mut notes = Vec::new();
    while let Some(row) = rows.next()? {
        notes.push(NoteRecord::from_row(row)?);
    }

    Ok(notes)
}

||||||| parent of 3a1bb2f (refactor: migrate to diesel-rs)
/// Select Note's matching the `NoteId` using the given [Connection].
///
/// # Returns
///
/// - Empty vector if no matching `note`.
/// - Otherwise, notes which `note_id` matches the `NoteId` as bytes.
pub fn select_notes_by_id(
    transaction: &Transaction,
    note_ids: &[NoteId],
) -> Result<Vec<NoteRecord>> {
    let note_ids: Vec<Value> = note_ids.iter().map(|id| id.to_bytes().into()).collect();

    let mut stmt = transaction.prepare_cached(&format!(
        "SELECT {}
        FROM notes
        LEFT JOIN note_scripts ON notes.script_root = note_scripts.script_root
        WHERE note_id IN rarray(?1)",
        NoteRecord::SELECT_COLUMNS
    ))?;
    let mut rows = stmt.query(params![Rc::new(note_ids)])?;

    let mut notes = Vec::new();
    while let Some(row) = rows.next()? {
        notes.push(NoteRecord::from_row(row)?);
    }

    Ok(notes)
}

=======
>>>>>>> 3a1bb2f (refactor: migrate to diesel-rs)
/// Select note inclusion proofs matching the `NoteId`, using the given [Connection].
///
/// # Returns
///
/// - Empty map if no matching `note`.
/// - Otherwise, note inclusion proofs, which `note_id` matches the `NoteId` as bytes.
pub fn select_note_inclusion_proofs(
    transaction: &Transaction,
    note_ids: BTreeSet<NoteId>,
) -> Result<BTreeMap<NoteId, NoteInclusionProof>> {
    QueryParamNoteIdLimit::check(note_ids.len())?;

    let note_ids: Vec<Value> = note_ids.into_iter().map(|id| id.to_bytes().into()).collect();

    let mut select_notes_stmt = transaction.prepare_cached(
        "
        SELECT
            block_num,
            note_id,
            batch_index,
            note_index,
            inclusion_path
        FROM
            notes
        WHERE
            note_id IN rarray(?1)
        ORDER BY
            block_num ASC
        ",
    )?;

    let mut result = BTreeMap::new();
    let mut rows = select_notes_stmt.query(params![Rc::new(note_ids)])?;
    while let Some(row) = rows.next()? {
        let block_num: u32 = row.get(0)?;

        let note_id_data = row.get_ref(1)?.as_blob()?;
        let note_id = NoteId::read_from_bytes(note_id_data)?;

        let batch_index = row.get(2)?;
        let note_index = row.get(3)?;
        // SAFETY: We can assume the batch and note indices stored in the DB are valid so this
        // should never panic.
        let node_index_in_block = BlockNoteIndex::new(batch_index, note_index)
            .expect("batch and note index from DB should be valid")
            .leaf_index_value();

        let inclusion_path_data = row.get_ref(4)?.as_blob()?;
        let inclusion_path = SparseMerklePath::read_from_bytes(inclusion_path_data)?;

        let proof = NoteInclusionProof::new(block_num.into(), node_index_in_block, inclusion_path)?;

        result.insert(note_id, proof);
    }

    Ok(result)
}

/// Returns a paginated batch of network notes that have not yet been consumed.
///
/// # Returns
///
/// A set of unconsumed network notes with maximum length of `size` and the page to get
/// the next set.
pub fn unconsumed_network_notes(
    transaction: &Transaction,
    mut page: Page,
) -> Result<(Vec<NoteRecord>, Page)> {
    assert_eq!(
        NoteExecutionMode::Network as u8,
        0,
        "Hardcoded execution value must match query"
    );

    // Select the rowid column so that we can return a pagination token.
    //
    // rowid column _must_ come after the note fields so that we don't mess up the
    // `NoteRecord::from_row` call.
    let mut stmt = transaction.prepare_cached(&format!(
        "
        SELECT {}, rowid
        FROM notes
        LEFT JOIN note_scripts ON notes.script_root = note_scripts.script_root
        WHERE
            execution_mode = 0 AND consumed = FALSE AND rowid >= ?
        ORDER BY rowid
        LIMIT ?
        ",
        "dummy"
    ))?;

    // The `page.size` is the maximum number of notes to return. We add 1 to it so that we can
    // check if there are more notes for the next page.
    let mut rows = stmt.query(params![page.token.unwrap_or(0), page.size.get() + 1])?;

    page.token = None;
    let mut notes = Vec::with_capacity(page.size.into());
    while let Some(row) = rows.next()? {
        if notes.len() == page.size.get() {
            page.token = Some(row.get::<_, u64>(14)?);
            break;
        }
        notes.push(todo!());
    }

    Ok((notes, page))
}

// BLOCK CHAIN QUERIES
// ================================================================================================

/// Insert a [`BlockHeader`] to the DB using the given [Transaction].
///
/// # Returns
///
/// The number of affected rows.
///
/// # Note
///
/// The [Transaction] object is not consumed. It's up to the caller to commit or rollback the
/// transaction.
pub fn insert_block_header(transaction: &Transaction, block_header: &BlockHeader) -> Result<usize> {
    let mut stmt =
        transaction.prepare_cached(insert_sql!(block_headers { block_num, block_header }))?;
    Ok(stmt.execute(params![block_header.block_num().as_u32(), block_header.to_bytes()])?)
}

/// Select a [`BlockHeader`] from the DB by its `block_num` using the given [Connection].
///
/// # Returns
///
/// When `block_number` is [None], the latest block header is returned. Otherwise, the block with
/// the given block height is returned.
pub fn select_block_header_by_block_num(
    transaction: &Transaction,
    block_number: Option<BlockNumber>,
) -> Result<Option<BlockHeader>> {
    let mut stmt;
    let mut rows = if let Some(block_number) = block_number {
        stmt = transaction
            .prepare_cached("SELECT block_header FROM block_headers WHERE block_num = ?1")?;
        stmt.query([block_number.as_u32()])?
    } else {
        stmt = transaction.prepare_cached(
            "SELECT block_header FROM block_headers ORDER BY block_num DESC LIMIT 1",
        )?;
        stmt.query([])?
    };

    match rows.next()? {
        Some(row) => {
            let data = row.get_ref(0)?.as_blob()?;
            Ok(Some(BlockHeader::read_from_bytes(data)?))
        },
        None => Ok(None),
    }
}

/// Select all the given block headers from the DB using the given [Connection].
///
/// # Note
///
/// Only returns the block headers that are actually present.
///
/// # Returns
///
/// A vector of [`BlockHeader`] or an error.
pub fn select_block_headers(
    transaction: &Transaction,
    blocks: impl Iterator<Item = BlockNumber> + Send,
) -> Result<Vec<BlockHeader>> {
    // The iterators are all deterministic, so is the conjunction.
    // All calling sites do it equivalently, hence the below holds.
    // <https://doc.rust-lang.org/src/core/slice/iter/macros.rs.html#195>
    // <https://doc.rust-lang.org/src/core/option.rs.html#2273>
    // And the conjunction is truthful:
    // <https://doc.rust-lang.org/src/core/iter/adapters/chain.rs.html#184>
    QueryParamBlockLimit::check(blocks.size_hint().0)?;

    let blocks: Vec<Value> = blocks.map(|b| b.as_u32().into()).collect();

    let mut headers = Vec::with_capacity(blocks.len());
    let mut stmt = transaction
        .prepare_cached("SELECT block_header FROM block_headers WHERE block_num IN rarray(?1);")?;
    let mut rows = stmt.query(params![Rc::new(blocks)])?;

    while let Some(row) = rows.next()? {
        let header = row.get_ref(0)?.as_blob()?;
        let header = BlockHeader::read_from_bytes(header)?;
        headers.push(header);
    }

    Ok(headers)
}

/// Select all block headers from the DB using the given [Connection].
///
/// # Returns
///
/// A vector of [`BlockHeader`] or an error.
pub fn select_all_block_headers(transaction: &Transaction) -> Result<Vec<BlockHeader>> {
    let mut stmt = transaction
        .prepare_cached("SELECT block_header FROM block_headers ORDER BY block_num ASC;")?;
    let mut rows = stmt.query([])?;
    let mut result = vec![];
    while let Some(row) = rows.next()? {
        let block_header_data = row.get_ref(0)?.as_blob()?;
        let block_header = BlockHeader::read_from_bytes(block_header_data)?;
        result.push(block_header);
    }

    Ok(result)
}

// TRANSACTIONS QUERIES
// ================================================================================================

/// Insert transactions to the DB using the given [Transaction].
///
/// # Returns
///
/// The number of affected rows.
///
/// # Note
///
/// The [Transaction] object is not consumed. It's up to the caller to commit or rollback the
/// transaction.
pub fn insert_transactions(
    transaction: &Transaction,
    block_num: BlockNumber,
    transactions: &OrderedTransactionHeaders,
) -> Result<usize> {
    let mut stmt = transaction.prepare_cached(insert_sql!(transactions {
        transaction_id,
        account_id,
        block_num
    }))?;
    let mut count = 0;
    for tx in transactions.as_slice() {
        count += stmt.execute(params![
            tx.id().to_bytes(),
            tx.account_id().to_bytes(),
            block_num.as_u32()
        ])?;
    }
    Ok(count)
}

/// Select transaction IDs from the DB using the given [Connection], filtered by account IDS,
/// given that the account updates were done between `(block_start, block_end]`.
///
/// # Returns
///
/// A vector of [`TransactionSummary`] containing the account ID, block number, and transaction ID
/// for each transaction that matches the criteria.
pub fn select_transactions_by_accounts_and_block_range(
    transaction: &Transaction,
    block_start: BlockNumber,
    block_end: BlockNumber,
    account_ids: &[AccountId],
) -> Result<Vec<TransactionSummary>> {
    QueryParamAccountIdLimit::check(account_ids.len())?;

    let account_ids: Vec<Value> = account_ids
        .iter()
        .copied()
        .map(|account_id| account_id.to_bytes().into())
        .collect();

    let mut stmt = transaction.prepare_cached(
        "
        SELECT
            account_id,
            block_num,
            transaction_id
        FROM
            transactions
        WHERE
            block_num > ?1 AND
            block_num <= ?2 AND
            account_id IN rarray(?3)
        ORDER BY
            transaction_id ASC
    ",
    )?;

    let mut rows =
        stmt.query(params![block_start.as_u32(), block_end.as_u32(), Rc::new(account_ids)])?;

    let mut result = vec![];
    while let Some(row) = rows.next()? {
        let account_id = read_from_blob_column(row, 0)?;
        let block_num = read_block_number(row, 1)?;
        let transaction_id_data = row.get_ref(2)?.as_blob()?;
        let transaction_id = TransactionId::read_from_bytes(transaction_id_data)?;

        result.push(TransactionSummary { account_id, block_num, transaction_id });
    }

    Ok(result)
}

// STATE SYNC
// ================================================================================================

/// Loads the state necessary for a state sync.
pub fn get_state_sync(
    transaction: &Transaction,
    block_num: BlockNumber,
    account_ids: &[AccountId],
    note_tag_prefixes: &[u32],
) -> Result<StateSyncUpdate, StateSyncError> {
    let notes = select_notes_since_block_by_tag_and_sender(
        transaction,
        note_tag_prefixes,
        account_ids,
        block_num,
    )?;

    let block_header =
        select_block_header_by_block_num(transaction, notes.first().map(|note| note.block_num))?
            .ok_or(StateSyncError::EmptyBlockHeadersTable)?;

    let account_updates = select_accounts_by_block_range(
        transaction,
        block_num,
        block_header.block_num(),
        account_ids,
    )?;

    let transactions = select_transactions_by_accounts_and_block_range(
        transaction,
        block_num,
        block_header.block_num(),
        account_ids,
    )?;

    Ok(StateSyncUpdate {
        notes,
        block_header,
        account_updates,
        transactions,
    })
}

// NOTE SYNC
// ================================================================================================

/// Loads the data necessary for a note sync.
pub fn get_note_sync(
    transaction: &Transaction,
    block_num: BlockNumber,
    note_tags: &[u32],
) -> Result<NoteSyncUpdate, NoteSyncError> {
    QueryParamNoteTagLimit::check(note_tags.len()).map_err(DatabaseError::from)?;
    let notes = select_notes_since_block_by_tag_and_sender(transaction, note_tags, &[], block_num)?;

    let block_header =
        select_block_header_by_block_num(transaction, notes.first().map(|note| note.block_num))?
            .ok_or(NoteSyncError::EmptyBlockHeadersTable)?;

    Ok(NoteSyncUpdate { notes, block_header })
}

// APPLY BLOCK
// ================================================================================================

/// Updates the DB with the state of a new block.
///
/// # Returns
///
/// The number of affected rows
/// in the DB.
pub fn apply_block(
    transaction: &Transaction,
    block_header: &BlockHeader,
    notes: &[(NoteRecord, Option<Nullifier>)],
    nullifiers: &[Nullifier],
    accounts: &[BlockAccountUpdate],
    transactions: &OrderedTransactionHeaders,
) -> Result<usize> {
    let mut count = 0;
    // Note: ordering here is important as the relevant tables have FK dependencies.
    count += insert_block_header(transaction, block_header)?;
    count += upsert_accounts(transaction, accounts, block_header.block_num())?;
    count += insert_scripts(transaction, notes.iter().map(|(note, _)| note))?;
    count += insert_notes(transaction, notes)?;
    count += insert_transactions(transaction, block_header.block_num(), transactions)?;
    count += insert_nullifiers_for_block(transaction, nullifiers, block_header.block_num())?;
    Ok(count)
}
