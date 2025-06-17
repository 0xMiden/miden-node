//! Defines models for usage with the diesel API
//!
//! Note: `select` can either be used as
//! `SelectDsl::select(schema::foo::table, (schema::foo::some_cool_id, ))`
//! or
//! `SelectDsl::select(schema::foo::table, FooRawRow::as_selectable())`.
//!
//! The former can be used to avoid declaring extra types, while the latter
//! is better if a full row is in need of loading and avoids duplicate
//! specification.
//!
//! Note: The fully qualified syntax yields for _much_ better errors.
//! The first step in debugging should always be using the fully qualified
//! calling syntext when dealing with diesel.

// TODO provide helper functions to limit the scope of these
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_lossless)]

use diesel::{prelude::*, sqlite::Sqlite};

use crate::{
    db::{
        NoteRecord, NoteSyncRecord, NullifierInfo,
        schema::{
            // the list of tables
            // referenced in `#[diesel(table_name = _)]`
            accounts,
            block_headers,
            notes,
            nullifiers,
            transactions,
        },
    },
    errors::DatabaseError,
};

fn raw_sql_to_block_number(raw: impl Into<i64>) -> BlockNumber {
    let raw = raw.into();
    #[allow(clippy::cast_sign_loss)]
    BlockNumber::from(raw as u32)
}

#[derive(Debug, Clone, Queryable, QueryableByName, Selectable)]
#[diesel(table_name = accounts)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct AccountRaw {
    pub account_id: Vec<u8>,
    #[allow(dead_code)]
    pub network_account_id_prefix: Option<i64>,
    pub account_commitment: Vec<u8>,
    pub block_num: i64,
    pub details: Option<Vec<u8>>,
}

use miden_lib::utils::{Deserializable, DeserializationError, Serializable};
use miden_node_proto::{self as proto, domain::account::AccountSummary};
use miden_objects::{
    Felt, Word,
    account::{Account, AccountId},
    block::{BlockHeader, BlockNoteIndex, BlockNumber},
    crypto::{hash::rpo::RpoDigest, merkle::MerklePath},
    note::{
        NoteAssets, NoteDetails, NoteExecutionHint, NoteInputs, NoteMetadata, NoteRecipient,
        NoteScript, NoteTag, NoteType, Nullifier,
    },
    transaction::TransactionId,
};

impl TryInto<proto::domain::account::AccountInfo> for AccountRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<proto::domain::account::AccountInfo, Self::Error> {
        use proto::domain::account::{AccountInfo, AccountSummary};
        let account_id = AccountId::read_from_bytes(&self.account_id[..])?;
        let account_commitment = RpoDigest::read_from_bytes(&self.account_commitment[..])?;
        let block_num = raw_sql_to_block_number(self.block_num);
        let summary = AccountSummary {
            account_id,
            account_commitment,
            block_num,
        };
        let details = self.details.as_deref().map(Account::read_from_bytes).transpose()?;
        Ok(AccountInfo { summary, details })
    }
}

#[derive(Debug, Clone, Queryable, QueryableByName, Selectable)]
#[diesel(table_name = nullifiers)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct NullifierRawRow {
    pub nullifier: Vec<u8>,
    #[allow(dead_code)]
    pub nullifier_prefix: i32, // TODO most usecases do not require this to be actually loaded
    pub block_num: i64,
}

impl TryInto<NullifierInfo> for NullifierRawRow {
    type Error = DatabaseError;
    fn try_into(self) -> Result<NullifierInfo, Self::Error> {
        let nullifier = Nullifier::read_from_bytes(&self.nullifier)?;
        let block_num = raw_sql_to_block_number(self.block_num);
        Ok(NullifierInfo { nullifier, block_num })
    }
}

#[derive(Debug, Clone, Queryable, QueryableByName, Selectable)]
#[diesel(table_name = block_headers)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct BlockHeaderRawRow {
    #[allow(dead_code)]
    pub block_num: i64,
    pub block_header: Vec<u8>,
}
impl TryInto<BlockHeader> for BlockHeaderRawRow {
    type Error = DatabaseError;
    fn try_into(self) -> Result<BlockHeader, Self::Error> {
        let block_header = BlockHeader::read_from_bytes(&self.block_header[..])?;
        Ok(block_header)
    }
}

#[derive(Debug, Clone, PartialEq, Queryable, Selectable, QueryableByName)]
#[diesel(table_name = transactions)]
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
            block_num: raw_sql_to_block_number(self.block_num),
            transaction_id: TransactionId::read_from_bytes(&self.transaction_id[..])?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Selectable, Queryable, QueryableByName)]
#[diesel(table_name = notes)]
#[diesel(check_for_backend(Sqlite))]
pub struct NoteMetadataRaw {
    note_type: i32,
    sender: Vec<u8>, // AccountId
    tag: i32,
    execution_hint: i64,
    aux: i64,
}

#[allow(clippy::cast_sign_loss)]
impl TryInto<NoteMetadata> for NoteMetadataRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<NoteMetadata, Self::Error> {
        let sender = AccountId::read_from_bytes(&self.sender[..])?;
        let note_type = NoteType::try_from(self.note_type as u32).expect("XXX");
        let tag = NoteTag::from(self.tag as u32);
        let execution_hint = NoteExecutionHint::try_from(self.execution_hint as u64).expect("XXX");
        let aux = Felt::new(self.aux as u64);
        Ok(NoteMetadata::new(sender, note_type, tag, execution_hint, aux)?)
    }
}

#[derive(Debug, Clone, PartialEq, Selectable, Queryable, QueryableByName)]
#[diesel(table_name = notes)]
#[diesel(check_for_backend(Sqlite))]
pub struct BlockNoteIndexRaw {
    pub batch_index: i32,
    pub note_index: i32, // index within batch
}

impl TryInto<BlockNoteIndex> for BlockNoteIndexRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<BlockNoteIndex, Self::Error> {
        Ok(BlockNoteIndex::new(self.batch_index as usize, self.batch_index as usize)
            .expect("XXX TODO"))
    }
}

#[derive(Debug, Clone, PartialEq, Selectable, Queryable, QueryableByName)]
#[diesel(table_name = notes)]
#[diesel(check_for_backend(Sqlite))]
pub struct NoteSyncRecordRawRow {
    pub block_num: i64, // BlockNumber
    #[diesel(embed)]
    pub block_note_index: BlockNoteIndexRaw,
    pub note_id: Vec<u8>, // BlobDigest
    #[diesel(embed)]
    pub metadata: NoteMetadataRaw,
    pub merkle_path: Vec<u8>, // MerklePath
}

impl TryInto<NoteSyncRecord> for NoteSyncRecordRawRow {
    type Error = DatabaseError;
    fn try_into(self) -> Result<NoteSyncRecord, Self::Error> {
        let block_num = raw_sql_to_block_number(self.block_num);
        let note_index = BlockNoteIndex::new(
            self.block_note_index.batch_index as usize,
            self.block_note_index.note_index as usize,
        )
        .expect("XXX"); // XXX usize is broken, adn we need to handle the error here better

        let note_id = RpoDigest::read_from_bytes(&self.note_id[..])?;
        let merkle_path = MerklePath::read_from_bytes(&self.merkle_path[..])?;
        let metadata = self.metadata.try_into()?;
        Ok(NoteSyncRecord {
            block_num,
            note_index,
            note_id,
            metadata,
            merkle_path,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Selectable, Queryable, QueryableByName)]
#[diesel(table_name = accounts)]
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
        let account_commitment = RpoDigest::read_from_bytes(&self.account_commitment[..])?;
        let block_num = raw_sql_to_block_number(self.block_num);

        Ok(AccountSummary {
            account_id,
            account_commitment,
            block_num,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Selectable, Queryable, QueryableByName)]
#[diesel(table_name = notes)]
#[diesel(check_for_backend(Sqlite))]
pub struct NoteDetailsRaw {
    pub assets: Option<Vec<u8>>,
    pub inputs: Option<Vec<u8>>,
    pub serial_num: Option<Vec<u8>>,
}

#[derive(diesel::QueryableByName, Debug)]
#[diesel(table_name = notes)] // Link to the notes table
pub struct NoteRecordRawNoResolve {
    pub block_num: i64,
    pub batch_index: i32,
    pub note_index: i32,
    pub note_id: Vec<u8>,
    pub note_type: i32,
    pub sender: Vec<u8>,
    pub tag: i32,
    pub execution_mode: i32,
    pub aux: i64,
    pub execution_hint: i64,
    pub merkle_path: Vec<u8>,
    pub consumed: i32, // Diesel maps bool to INTEGER (0 for false, 1 for true)
    pub nullifier: Option<Vec<u8>>,
    pub assets: Option<Vec<u8>>,
    pub inputs: Option<Vec<u8>>,
    pub script_root: Option<Vec<u8>>,
    pub serial_num: Option<Vec<u8>>,
    // Add the rowid field here, implicit in SQLite! It causes all kinds of havoc
    // pub rowid: i64,
}

impl TryInto<NoteRecord> for NoteRecordRawNoResolve {
    type Error = DatabaseError;
    fn try_into(self) -> Result<NoteRecord, Self::Error> {
        let NoteRecordRawNoResolve {
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
            merkle_path,
            consumed,
            nullifier,
            assets,
            inputs,
            script_root,
            serial_num,
            // rowid,
        } = self;

        let index = BlockNoteIndexRaw { batch_index, note_index };
        let metadata = NoteMetadataRaw {
            note_type,
            sender,
            tag,
            execution_hint,
            aux,
        };
        let details = NoteDetailsRaw { assets, inputs, serial_num };

        let metadata = metadata.try_into()?;
        let block_num = raw_sql_to_block_number(block_num);
        let note_id = RpoDigest::read_from_bytes(&note_id[..])?;
        let details = None; // TODO check against origin NoteRecord::from_raw

        let merkle_path = MerklePath::read_from_bytes(&merkle_path[..])?;
        let note_index = index.try_into()?;
        Ok(NoteRecord {
            block_num,
            note_index,
            note_id,
            metadata,
            details,
            merkle_path,
        })
    }
}

// Note: One cannot use `#[diesel(embed)]` to structure
// this, it will yield a significant amount of errors
// when used with join and debugging is painful to put it
// mildly.
#[derive(Debug, Clone, PartialEq, Queryable)]
pub struct NoteRecordRaw {
    pub block_num: i64,

    pub batch_index: i32,
    pub note_index: i32, // index within batch
    // #[diesel(embed)]
    // pub note_index: BlockNoteIndexRaw,
    pub note_id: Vec<u8>,

    pub note_type: i32,
    pub sender: Vec<u8>, // AccountId
    pub tag: i32,
    pub execution_hint: i64,
    pub aux: i64,
    // #[diesel(embed)]
    // pub metadata: NoteMetadataRaw,
    pub assets: Option<Vec<u8>>,
    pub inputs: Option<Vec<u8>>,
    pub serial_num: Option<Vec<u8>>,

    // #[diesel(embed)]
    // pub details: NoteDetailsRaw,
    pub merkle_path: Vec<u8>,
    pub script: Option<Vec<u8>>, // not part of notes::table!
}

impl TryInto<NoteRecord> for NoteRecordRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<NoteRecord, Self::Error> {
        // let (raw, script) = self;
        let raw = self;
        let NoteRecordRaw {
            block_num,

            batch_index,
            note_index,
            // block note index ^^^
            note_id,

            note_type,
            sender,
            tag,
            execution_hint,
            aux,
            // metadata ^^^,
            assets,
            inputs,
            serial_num,
            //details ^^^,
            merkle_path,
            script,
            ..
        } = raw;
        let index = BlockNoteIndexRaw { batch_index, note_index };
        let metadata = NoteMetadataRaw {
            note_type,
            sender,
            tag,
            execution_hint,
            aux,
        };
        let details = NoteDetailsRaw { assets, inputs, serial_num };

        let metadata = metadata.try_into()?;
        let block_num = raw_sql_to_block_number(block_num);
        let note_id = RpoDigest::read_from_bytes(&note_id[..])?;
        let script = script.map(|script| NoteScript::read_from_bytes(&script[..])).transpose()?;
        let details = if let NoteDetailsRaw {
            assets: Some(assets),
            inputs: Some(inputs),
            serial_num: Some(serial_num),
        } = details
        {
            let inputs = NoteInputs::read_from_bytes(&inputs[..])?;
            let serial_num = Word::read_from_bytes(&serial_num[..])?;
            let recipient = NoteRecipient::new(serial_num, script.expect("XXX TODO"), inputs);
            let assets = NoteAssets::read_from_bytes(&assets[..])?;
            Some(NoteDetails::new(assets, recipient))
        } else {
            None
        };
        let merkle_path = MerklePath::read_from_bytes(&merkle_path[..])?;
        let note_index = index.try_into()?;
        Ok(NoteRecord {
            block_num,
            note_index,
            note_id,
            metadata,
            details,
            merkle_path,
        })
    }
}

/// Utility to convert an iteratable container of containing `R`-typed values
/// to a `Vec<D>` and bail at the first failing conversion
pub fn vec_raw_try_into<D, R: TryInto<D>>(
    raw: impl IntoIterator<Item = R>,
) -> std::result::Result<Vec<D>, <R as TryInto<D>>::Error> {
    std::result::Result::<Vec<D>, <R as TryInto<D>>::Error>::from_iter(
        raw.into_iter().map(<R as std::convert::TryInto<D>>::try_into),
    )
}

#[allow(dead_code)]
/// Deserialze an iteratable container full of byte blobs `B` to types `T`
pub fn deserialize_raw_vec<B: AsRef<[u8]>, T: Deserializable>(
    raw: impl IntoIterator<Item = B>,
) -> std::result::Result<Vec<T>, DeserializationError> {
    std::result::Result::<Vec<_>, DeserializationError>::from_iter(
        raw.into_iter().map(|raw| T::read_from_bytes(raw.as_ref())),
    )
}

/// Utility to convert an iteratable container to a vector of byte blobs
pub fn serialize_vec<'a, D: Serializable + 'a>(
    raw: impl IntoIterator<Item = &'a D>,
) -> Vec<Vec<u8>> {
    Vec::<_>::from_iter(raw.into_iter().map(<D as Serializable>::to_bytes))
}

// TODO move
pub mod queries {
    use std::{
        borrow::Cow,
        collections::{BTreeMap, BTreeSet},
    };

    use diesel::{
        JoinOnDsl, NullableExpressionMethods, OptionalExtension, SqliteConnection, alias,
        query_dsl::methods::SelectDsl,
    };
    use miden_lib::utils::{Deserializable, Serializable};
    use miden_node_proto::domain::account::NetworkAccountPrefix;
    use miden_objects::{
        Word,
        account::{
            Account, AccountDelta, AccountStorageDelta, AccountVaultDelta, FungibleAssetDelta,
            NonFungibleAssetDelta, NonFungibleDeltaAction, StorageMapDelta,
            delta::AccountUpdateDetails,
        },
        asset::NonFungibleAsset,
        block::{BlockAccountUpdate, BlockHeader, BlockNoteIndex},
        crypto::{
            hash::rpo::RpoDigest,
            merkle::MerklePath,
        },
        note::{
            NoteExecutionMode, NoteId, NoteInclusionProof, Nullifier,
        },
        transaction::OrderedTransactionHeaders,
    };

    use super::{
        super::models, AccountId, BlockNumber, BoolExpressionMethods, DatabaseError,
        ExpressionMethods, NoteSyncRecordRawRow, QueryDsl, RunQueryDsl, SelectableHelper,
        serialize_vec,
    };
    use crate::db::{
        NoteRecord, NoteSyncUpdate, Page,
        models::{
            NoteRecordRawNoResolve, deserialize_raw_vec, vec_raw_try_into,
        },
        schema,
        sql::utils::get_nullifier_prefix,
    };

    pub fn select_notes_since_block_by_tag_and_sender(
        conn: &mut SqliteConnection,
        block_number: BlockNumber,
        account_ids: &[AccountId],
        note_tags: &[u32],
    ) -> Result<NoteSyncUpdate, DatabaseError> {
        let desired_note_tags =
            Vec::from_iter(note_tags.iter().map(|tag| i32::from_be_bytes(tag.to_be_bytes())));
        let desired_senders = serialize_vec(account_ids.iter());

        // select notes since block by tag and sender
        let desired_block_num: i64 =
            SelectDsl::select(schema::notes::table, schema::notes::block_num)
                .filter(
                    schema::notes::tag
                        .eq_any(&desired_note_tags[..])
                        .or(schema::notes::sender.eq_any(&desired_senders[..])),
                )
                .order_by(schema::notes::block_num.asc())
                .limit(1)
                .get_result(conn)
                .optional()?
                .unwrap(); // TODO FIXME
        let block_header = select_block_header_by_block_num(conn, Some(block_number))?.unwrap(); // TODO FIXME

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

        Ok(NoteSyncUpdate {
            notes: vec_raw_try_into(notes)?,
            block_header,
        })
    }

    pub fn select_block_header_by_block_num(
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

    pub fn select_note_inclusion_proofs(
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

    pub fn insert_block_header(
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
    pub fn apply_delta(
        _account_id: AccountId, // TODO error handline shifted _outside_
        mut account: Account,
        delta: &AccountDelta,
        final_state_commitment: &RpoDigest,
    ) -> crate::db::Result<Account, DatabaseError> {
        // TODO former error handling
        // let account = value.as_blob_or_null()?;
        // let account = account.map(Account::read_from_bytes).transpose()?;

        // let Some(mut account) = account else {
        //     return Err(DatabaseError::AccountNotPublic(account_id));
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
    fn insert_account_delta(
        conn: &mut SqliteConnection,
        account_id: AccountId,
        block_number: BlockNumber,
        delta: &AccountDelta,
    ) -> Result<(), DatabaseError> {
        let insert_acc_delta_stmt = |conn2: &mut SqliteConnection,
                                     account_id: AccountId,
                                     block_num: BlockNumber,
                                     nonce: u32|
         -> Result<usize, DatabaseError> {
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

        let insert_fungible_asset_delta_stmt = |conn2: &mut SqliteConnection,
                                                account_id: AccountId,
                                                block_num: BlockNumber,
                                                faucet_id: Vec<u8>,
                                                delta: u32|
         -> Result<usize, DatabaseError> {
            let count = diesel::insert_into(schema::account_fungible_asset_deltas::table)
                .values(&[(
                    schema::account_fungible_asset_deltas::account_id.eq(account_id.to_bytes()),
                    schema::account_fungible_asset_deltas::block_num.eq(block_num.as_u32() as i64),
                    schema::account_fungible_asset_deltas::faucet_id.eq(faucet_id),
                    schema::account_fungible_asset_deltas::delta.eq(delta as i32),
                )])
                .execute(conn2)?;
            Ok(count)
        };

        let insert_non_fungible_asset_update_stmt = |conn2: &mut SqliteConnection,
                                                     account_id: AccountId,
                                                     block_num: BlockNumber,
                                                     vault_key: Vec<u8>,
                                                     is_remove: i32|
         -> Result<usize, DatabaseError> {
            let count = diesel::insert_into(schema::account_non_fungible_asset_updates::table)
                .values(&[(
                    schema::account_non_fungible_asset_updates::account_id
                        .eq(account_id.to_bytes()),
                    schema::account_non_fungible_asset_updates::block_num
                        .eq(block_num.as_u32() as i64),
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
    #[allow(clippy::too_many_lines)]
    pub fn upsert_accounts(
        conn: &mut SqliteConnection,
        accounts: &[BlockAccountUpdate],
        block_num: BlockNumber,
    ) -> Result<usize, DatabaseError> {
        let select_details_stmt = |conn2: &mut SqliteConnection,
                                   account_id: AccountId|
         -> Result<Vec<Account>, DatabaseError> {
            let account_id = account_id.to_bytes();
            let account_details_serialized = SelectDsl::select(
                schema::accounts::table,
                schema::accounts::details.assume_not_null(),
            )
            .filter(schema::accounts::account_id.eq(account_id))
            .filter(schema::accounts::details.is_not_null())
            .get_results::<Vec<u8>>(conn2)?;
            let accounts = deserialize_raw_vec::<_, Account>(account_details_serialized)?;
            Ok(accounts)
        };

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
                schema::accounts::details
                    .eq(full_account.as_ref().map(|account| account.to_bytes())),
            );
            let inserted = diesel::insert_into(schema::accounts::table)
                .values(&val)
                // TODO do the update on conflict
                // .on_conflict(schema::accounts::account_id)
                // .do_update()
                // .set(&val)
                .execute(conn)?;

            debug_assert_eq!(inserted, 1);

            if let Some(delta) = insert_delta {
                insert_account_delta(conn, account_id, block_num, &delta)?;
            }

            count += inserted;
        }

        Ok(count)
    }

    pub fn insert_scripts<'a>(
        conn: &mut SqliteConnection,
        notes: impl IntoIterator<Item = &'a NoteRecord>,
    ) -> Result<usize, DatabaseError> {
        let count = diesel::insert_into(schema::note_scripts::table)
            .values(Vec::from_iter(notes.into_iter().filter_map(|note| {
                let note_details = note.details.as_ref()?;
                Some((
                    schema::note_scripts::script_root.eq(note_details.script().root().to_bytes()),
                    schema::note_scripts::script.eq(note_details.script().to_bytes()),
                ))
            })))
            .execute(conn)?;

        Ok(count)
    }

    pub fn insert_notes(
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
                    schema::notes::execution_mode
                        .eq(note.metadata.tag().execution_mode() as u8 as i32),
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

    pub fn insert_transactions(
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

    pub fn insert_nullifiers_for_block(
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
                        schema::nullifiers::nullifier_prefix
                            .eq(get_nullifier_prefix(nullifier) as i32),
                        schema::nullifiers::block_num.eq(block_num.as_u32() as i64),
                    )
                },
            )))
            .execute(conn)?;

        Ok(count)
    }

    pub fn apply_block(
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

    pub fn unconsumed_network_notes(
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
            diesel::dsl::sql::<diesel::sql_types::Bool>("notes.rowid >= ? ")
                .bind::<diesel::sql_types::BigInt, i64>(page.token.unwrap_or_default() as i64);

        type RawLoadedTuple = (
            i64,             // block_num
            i32,             // batch_index
            i32,             // note_index
            Vec<u8>,         // note_id
            i32,             // note_type
            Vec<u8>,         // sender
            i32,             // tag
            i32,             // execution_mode
            i64,             // aux
            i64,             // execution_hint
            Vec<u8>,         // merkle_path
            i32,             // consumed
            Option<Vec<u8>>, // nullifier
            Option<Vec<u8>>, // assets
            Option<Vec<u8>>, // inputs
            Option<Vec<u8>>, // script_root
            Option<Vec<u8>>, // serial_num
            i64,             // rowid (from sql::<BigInt>("notes.rowid"))
        );

        fn hack(tuple: RawLoadedTuple) -> (NoteRecordRawNoResolve, i64) {
            (
                NoteRecordRawNoResolve {
                    block_num: tuple.0,
                    batch_index: tuple.1,
                    note_index: tuple.2,
                    note_id: tuple.3,
                    note_type: tuple.4,
                    sender: tuple.5,
                    tag: tuple.6,
                    execution_mode: tuple.7,
                    aux: tuple.8,
                    execution_hint: tuple.9,
                    merkle_path: tuple.10,
                    consumed: tuple.11,
                    nullifier: tuple.12,
                    assets: tuple.13,
                    inputs: tuple.14,
                    script_root: tuple.15,
                    serial_num: tuple.16,
                },
                tuple.17,
            )
        }

        // let paged = |conn: &mut SqliteConnection, offset: u64, limit: u64, dest: &mut
        // Vec<NoteRecord>| {
        let raw =
            SelectDsl::select(
                schema::notes::table.left_join(schema::note_scripts::table.on(
                    schema::notes::script_root.eq(schema::note_scripts::script_root.nullable()),
                )),
                (
                    schema::notes::block_num,
                    schema::notes::batch_index,
                    schema::notes::note_index,
                    schema::notes::note_id,
                    schema::notes::note_type,
                    schema::notes::sender,
                    schema::notes::tag,
                    schema::notes::execution_mode,
                    schema::notes::aux,
                    schema::notes::execution_hint,
                    schema::notes::merkle_path,
                    schema::notes::consumed,
                    schema::notes::nullifier,
                    schema::notes::assets,
                    schema::notes::inputs,
                    schema::notes::script_root,
                    schema::notes::serial_num,
                    rowid_sel.clone(),
                ),
            )
            .filter(
                schema::notes::execution_mode
                    .eq(0 as i32)
                    .and(schema::notes::consumed.eq(false as u8 as i32))
                    .and(rowid_sel_ge),
            )
            .order(rowid_sel.asc())
            .limit(page.size.get() as i64)
            .load::<RawLoadedTuple>(conn)?;

        // dest.extend(iter.map(|| todo!()))?;

        let mut notes = Vec::with_capacity(page.size.into());
        let mut iter = raw.into_iter();
        while let Some(raw_item) = iter.next() {
            let (raw_item, row) = hack(raw_item);
            page.token = None;
            if notes.len() == page.size.get() {
                page.token = Some(row as u64);
                break;
            }
            notes.push(TryInto::<NoteRecord>::try_into(raw_item)?);
        }

        Ok((notes, page))
    }

    pub fn select_account_delta(
        conn: &mut SqliteConnection,
        account_id: AccountId,
        block_start: BlockNumber,
        block_end: BlockNumber,
    ) -> Result<Option<AccountDelta>, DatabaseError> {
        let start_block_num = block_start.as_u32();
        let end_block_num = block_end.as_u32();

        let select_nonce_stmt = |conn: &mut SqliteConnection,
                                 account_id: &AccountId,
                                 start_block_num: &BlockNumber,
                                 end_block_num: &BlockNumber| {
            let desired_account_id = account_id.to_bytes();
            let start_block_num = start_block_num.as_u32() as i64;
            let end_block_num = end_block_num.as_u32() as i64;

            let res =
                SelectDsl::select(schema::account_deltas::table, schema::account_deltas::nonce)
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
            Ok::<Option<u64>, DatabaseError>(res.map(|nonce| nonce as u64)) // FIXME what type is nonce supposed to be
        };

        let select_slot_updates_stmt =
            |conn: &mut SqliteConnection,
             account_id_val: &AccountId,
             start_block_num: &BlockNumber,
             end_block_num: &BlockNumber| {
                let desired_account_id = account_id_val.to_bytes();
                let start_block_num = start_block_num.as_u32() as i64;
                let end_block_num = end_block_num.as_u32() as i64;

                use schema::account_storage_slot_updates::dsl::*;

                // Alias the table for the inner and outer query
                let (a, b) = alias!(
                    schema::account_storage_slot_updates as a,
                    schema::account_storage_slot_updates as b
                );

                // Construct the NOT EXISTS subquery
                let subquery =
                    b // the raw SQL uses SELECT 1, but diesel cannot represent that
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
                Ok::<_, DatabaseError>(results)
            };

        let select_storage_map_updates_stmt = |conn: &mut SqliteConnection,
                                               account_id_val: &AccountId,
                                               start_block_num: &BlockNumber,
                                               end_block_num: &BlockNumber|
         -> Result<
            Vec<(i32, Vec<u8>, Vec<u8>)>,
            DatabaseError,
        > {
            let desired_account_id = account_id_val.to_bytes();
            let start_block_num = start_block_num.as_u32() as i64;
            let end_block_num = end_block_num.as_u32() as i64;

            use schema::account_storage_map_updates::dsl::*;

            // Alias the table for the inner and outer query
            let (a, b) = alias!(
                schema::account_storage_map_updates as a,
                schema::account_storage_map_updates as b
            );

            // Construct the NOT EXISTS subquery
            let subquery = //diesel::dsl::select(1_i32.as_sql::<diesel::sql_types::Integer>())
                b // diesel cannot represent a lack of from query
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
        };

        let select_fungible_asset_deltas_stmt =
            |conn: &mut SqliteConnection,
             account_id: &AccountId,
             start_block_num: &BlockNumber,
             end_block_num: &BlockNumber|
             -> Result<Vec<(Vec<u8>, Option<i64>)>, DatabaseError> {
                let desired_account_id = account_id.to_bytes();
                let start_block_num = start_block_num.as_u32() as i64;
                let end_block_num = end_block_num.as_u32() as i64;

                Ok(SelectDsl::select(
                    schema::account_fungible_asset_deltas::table
                        .filter(
                            schema::account_fungible_asset_deltas::account_id
                                .eq(desired_account_id)
                                .and(
                                    schema::account_fungible_asset_deltas::block_num
                                        .gt(start_block_num),
                                )
                                .and(
                                    schema::account_fungible_asset_deltas::block_num
                                        .le(end_block_num),
                                ),
                        )
                        .group_by(schema::account_fungible_asset_deltas::faucet_id),
                    (
                        schema::account_fungible_asset_deltas::faucet_id,
                        diesel::dsl::sum(schema::account_fungible_asset_deltas::delta),
                    ),
                )
                .load(conn)?)
            };

        let select_non_fungible_asset_updates_stmt = |conn: &mut SqliteConnection,
                                                      account_id: &AccountId,
                                                      start_block_num: &BlockNumber,
                                                      end_block_num: &BlockNumber|
         -> Result<
            Vec<(i64, Vec<u8>, i32)>,
            DatabaseError,
        > {
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
        };

        let Some(nonce) = select_nonce_stmt(conn, &account_id, &block_start, &block_end)? else {
            return Ok(None);
        };
        let nonce = nonce.try_into().map_err(DatabaseError::InvalidFelt)?;

        let rows: Vec<(i32, Vec<u8>)> = // XXX double check types
            select_slot_updates_stmt(conn, &account_id, &block_start, &block_end)?;
        let storage_scalars =
            Result::<BTreeMap<u8, Word>, _>::from_iter(rows.iter().map(|(slot, value)| {
                let felt = Word::read_from_bytes(value)?;
                Ok::<_, DatabaseError>((*slot as u8, felt))
            }))?;

        let rows = select_storage_map_updates_stmt(conn, &account_id, &block_start, &block_end)?;
        // (slot, StorageDeltaMap)
        let mut storage_maps = BTreeMap::<u8, StorageMapDelta>::new();
        for (slot, key, value) in rows {
            let key = miden_objects::Digest::read_from_bytes(&key)?;
            let value = Word::read_from_bytes(&value)?;
            storage_maps.entry(slot as u8).or_default().insert(key, value); // FIXME cross check
        }

        let mut fungible = BTreeMap::<AccountId, i64>::new();
        let rows = select_fungible_asset_deltas_stmt(conn, &account_id, &block_start, &block_end)?;
        for (faucet_id, value) in rows {
            let faucet_id = AccountId::read_from_bytes(&faucet_id)?;
            fungible.insert(faucet_id, value.unwrap()); // FIXME XXX deal with failability
        }

        let mut non_fungible_delta = NonFungibleAssetDelta::default();
        let rows =
            select_non_fungible_asset_updates_stmt(conn, &account_id, &block_start, &block_end)?;
        for (_, vault_key_asset, action) in rows {
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
}
