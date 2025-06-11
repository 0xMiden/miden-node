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
pub struct AccountInfoRawRow {
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

impl TryInto<proto::domain::account::AccountInfo> for AccountInfoRawRow {
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

pub mod queries {
    use diesel::{OptionalExtension, SqliteConnection, query_dsl::methods::SelectDsl};
    use miden_objects::block::BlockHeader;

    use super::{
        super::models, AccountId, BlockNumber, BoolExpressionMethods, DatabaseError,
        ExpressionMethods, NoteSyncRecordRawRow, QueryDsl, RunQueryDsl, SelectableHelper,
        serialize_vec,
    };
    use crate::db::{NoteSyncUpdate, models::vec_raw_try_into, schema};

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
}
