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
        NoteSyncRecord, NullifierInfo,
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

use miden_lib::utils::{Deserializable, Serializable};
use miden_node_proto::{self as proto, domain::account::AccountSummary};
use miden_objects::{
    Felt,
    account::{Account, AccountId},
    block::{BlockHeader, BlockNoteIndex, BlockNumber},
    crypto::{hash::rpo::RpoDigest, merkle::MerklePath},
    note::{NoteExecutionHint, NoteMetadata, NoteTag, Nullifier},
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
    sender: Vec<u8>, // AccountId
    note_type: i32,
    tag: i32,
    execution_hint: i64,
    aux: i64,
}

#[allow(clippy::cast_sign_loss)]
impl TryInto<NoteMetadata> for NoteMetadataRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<NoteMetadata, Self::Error> {
        let sender = AccountId::read_from_bytes(&self.sender[..])?;
        let note_type =
            miden_objects::note::NoteType::try_from(self.note_type as u32).expect("XXX");
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

#[derive(Debug, Clone, PartialEq, Selectable, Queryable, QueryableByName)]
#[diesel(table_name = notes)]
#[diesel(check_for_backend(Sqlite))]
pub struct NoteSyncRecordRawRow {
    pub block_num: i64, // BlockNumber
    #[diesel(embed)]
    pub block_index: BlockNoteIndexRaw,
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
            self.block_index.batch_index as usize,
            self.block_index.note_index as usize,
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

/// Utility to convert an iteratable container of containing `R`-typed values
/// to a `Vec<D>` and bail at the first failing conversion
pub fn vec_raw_try_into<D, R: TryInto<D>>(
    raw: impl IntoIterator<Item = R>,
) -> std::result::Result<Vec<D>, <R as TryInto<D>>::Error> {
    std::result::Result::<Vec<_>, _>::from_iter(
        raw.into_iter().map(<R as std::convert::TryInto<D>>::try_into),
    )
}

/// Utility to convert an iteratable container to a vector of byte blobs
pub fn vec_to_raw<'a, D: Serializable + 'a>(raw: impl IntoIterator<Item = &'a D>) -> Vec<Vec<u8>> {
    Vec::<_>::from_iter(raw.into_iter().map(<D as Serializable>::to_bytes))
}
