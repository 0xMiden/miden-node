use bigdecimal::BigDecimal;
use diesel::prelude::Insertable;
use miden_lib::utils::{Deserializable, Serializable};
use miden_node_proto::{self as proto, domain::account::AccountSummary};
use miden_objects::{
    Felt, Word,
    account::{Account, AccountId},
    block::{BlockHeader, BlockNoteIndex, BlockNumber},
    crypto::merkle::SparseMerklePath,
    note::{
        NoteAssets, NoteDetails, NoteExecutionHint, NoteInputs, NoteMetadata, NoteRecipient,
        NoteScript, NoteTag, NoteType, Nullifier,
    },
    transaction::TransactionId,
};

use super::{
    DatabaseError, NoteRecord, NoteSyncRecord, NullifierInfo, Queryable, QueryableByName,
    Selectable, Sqlite, accounts, block_headers, notes, nullifiers, transactions,
};
use crate::db::models::conv::{
    DbReprConv, aux_to_raw_sql, consumed_to_raw_sql, execution_hint_to_raw_sql,
    execution_mode_to_raw_sql, idx_to_raw_sql, note_type_to_raw_sql,
};

#[derive(Debug, Clone, Queryable, QueryableByName, Selectable)]
#[diesel(table_name = accounts)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct AccountRaw {
    pub account_id: Vec<u8>,
    pub account_commitment: Vec<u8>,
    pub block_num: i64,
    pub details: Option<Vec<u8>>,
}

impl TryInto<proto::domain::account::AccountInfo> for AccountRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<proto::domain::account::AccountInfo, Self::Error> {
        use proto::domain::account::{AccountInfo, AccountSummary};
        let account_id = AccountId::read_from_bytes(&self.account_id[..])?;
        let account_commitment = Word::read_from_bytes(&self.account_commitment[..])?;
        let block_num = BlockNumber::from_raw_sql(self.block_num)?;
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
pub struct NullifierWithoutPrefixRawRow {
    pub nullifier: Vec<u8>,
    pub block_num: i64,
}

impl TryInto<NullifierInfo> for NullifierWithoutPrefixRawRow {
    type Error = DatabaseError;
    fn try_into(self) -> Result<NullifierInfo, Self::Error> {
        let nullifier = Nullifier::read_from_bytes(&self.nullifier)?;
        let block_num = BlockNumber::from_raw_sql(self.block_num)?;
        Ok(NullifierInfo { nullifier, block_num })
    }
}

#[derive(Debug, Clone, Queryable, QueryableByName, Selectable)]
#[diesel(table_name = block_headers)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct BlockHeaderRaw {
    #[allow(dead_code)]
    pub block_num: i64,
    pub block_header: Vec<u8>,
}
impl TryInto<BlockHeader> for BlockHeaderRaw {
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
            block_num: BlockNumber::from_raw_sql(self.block_num)?,
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
    aux: i64,
    execution_hint: i64,
}

#[allow(clippy::cast_sign_loss)]
impl TryInto<NoteMetadata> for NoteMetadataRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<NoteMetadata, Self::Error> {
        let sender = AccountId::read_from_bytes(&self.sender[..])?;
        let note_type = NoteType::try_from(self.note_type as u32)
            .map_err(DatabaseError::conversiont_from_sql::<NoteType, _, _>)?;
        let tag = NoteTag::from(self.tag as u32);
        let execution_hint = NoteExecutionHint::try_from(self.execution_hint as u64)
            .map_err(DatabaseError::conversiont_from_sql::<NoteExecutionHint, _, _>)?;
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

#[allow(clippy::cast_sign_loss, reason = "Indices are cast to usize for ease of use")]
impl TryInto<BlockNoteIndex> for BlockNoteIndexRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<BlockNoteIndex, Self::Error> {
        let batch_index = self.batch_index as usize;
        let note_index = self.note_index as usize;
        let index = BlockNoteIndex::new(batch_index, note_index).ok_or_else(|| {
            DatabaseError::conversiont_from_sql::<BlockNoteIndex, DatabaseError, _>(None)
        })?;
        Ok(index)
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
    pub inclusion_path: Vec<u8>, // SparseMerklePath
}

#[allow(clippy::cast_sign_loss, reason = "Indices are cast to usize for ease of use")]
impl TryInto<NoteSyncRecord> for NoteSyncRecordRawRow {
    type Error = DatabaseError;
    fn try_into(self) -> Result<NoteSyncRecord, Self::Error> {
        let block_num = BlockNumber::from_raw_sql(self.block_num)?;
        let note_index = self.block_note_index.try_into()?;

        let note_id = Word::read_from_bytes(&self.note_id[..])?;
        let inclusion_path = SparseMerklePath::read_from_bytes(&self.inclusion_path[..])?;
        let metadata = self.metadata.try_into()?;
        Ok(NoteSyncRecord {
            block_num,
            note_index,
            note_id,
            metadata,
            inclusion_path,
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
        let account_commitment = Word::read_from_bytes(&self.account_commitment[..])?;
        let block_num = BlockNumber::from_raw_sql(self.block_num)?;

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
pub struct NoteRecordWithScriptRaw {
    pub block_num: i64,

    pub batch_index: i32,
    pub note_index: i32, // index within batch
    // #[diesel(embed)]
    // pub note_index: BlockNoteIndexRaw,
    pub note_id: Vec<u8>,

    pub note_type: i32,
    pub sender: Vec<u8>, // AccountId
    pub tag: i32,
    pub aux: i64,
    pub execution_hint: i64,
    // #[diesel(embed)]
    // pub metadata: NoteMetadataRaw,
    pub assets: Option<Vec<u8>>,
    pub inputs: Option<Vec<u8>>,
    pub serial_num: Option<Vec<u8>>,

    // #[diesel(embed)]
    // pub details: NoteDetailsRaw,
    pub inclusion_path: Vec<u8>,
    pub script: Option<Vec<u8>>, // not part of notes::table!
}

impl From<(NoteRecordRaw, Option<Vec<u8>>)> for NoteRecordWithScriptRaw {
    fn from((note, script): (NoteRecordRaw, Option<Vec<u8>>)) -> Self {
        let NoteRecordRaw {
            block_num,
            batch_index,
            note_index,
            note_id,
            note_type,
            sender,
            tag,
            aux,
            execution_hint,
            assets,
            inputs,
            serial_num,
            inclusion_path,
        } = note;
        Self {
            block_num,
            batch_index,
            note_index,
            note_id,
            note_type,
            sender,
            tag,
            aux,
            execution_hint,
            assets,
            inputs,
            serial_num,
            inclusion_path,
            script,
        }
    }
}

impl TryInto<NoteRecord> for NoteRecordWithScriptRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<NoteRecord, Self::Error> {
        // let (raw, script) = self;
        let raw = self;
        let NoteRecordWithScriptRaw {
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
            inclusion_path,
            script,
            ..
        } = raw;
        let index = BlockNoteIndexRaw { batch_index, note_index };
        let metadata = NoteMetadataRaw {
            note_type,
            sender,
            tag,
            aux,
            execution_hint,
        };
        let details = NoteDetailsRaw { assets, inputs, serial_num };

        let metadata = metadata.try_into()?;
        let block_num = BlockNumber::from_raw_sql(block_num)?;
        let note_id = Word::read_from_bytes(&note_id[..])?;
        let script = script.map(|script| NoteScript::read_from_bytes(&script[..])).transpose()?;
        let details = if let NoteDetailsRaw {
            assets: Some(assets),
            inputs: Some(inputs),
            serial_num: Some(serial_num),
        } = details
        {
            let inputs = NoteInputs::read_from_bytes(&inputs[..])?;
            let serial_num = Word::read_from_bytes(&serial_num[..])?;
            let script = script.ok_or_else(|| {
                DatabaseError::conversiont_from_sql::<NoteRecipient, DatabaseError, _>(None)
            })?;
            let recipient = NoteRecipient::new(serial_num, script, inputs);
            let assets = NoteAssets::read_from_bytes(&assets[..])?;
            Some(NoteDetails::new(assets, recipient))
        } else {
            None
        };
        let inclusion_path = SparseMerklePath::read_from_bytes(&inclusion_path[..])?;
        let note_index = index.try_into()?;
        Ok(NoteRecord {
            block_num,
            note_index,
            note_id,
            metadata,
            details,
            inclusion_path,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Selectable, Queryable, QueryableByName)]
#[diesel(table_name = notes)]
#[diesel(check_for_backend(Sqlite))]
pub struct NoteRecordRaw {
    pub block_num: i64,

    pub batch_index: i32,
    pub note_index: i32, // index within batch
    pub note_id: Vec<u8>,

    pub note_type: i32,
    pub sender: Vec<u8>, // AccountId
    pub tag: i32,
    pub aux: i64,
    pub execution_hint: i64,

    pub assets: Option<Vec<u8>>,
    pub inputs: Option<Vec<u8>>,
    pub serial_num: Option<Vec<u8>>,

    pub inclusion_path: Vec<u8>,
}

/// A type to represent a `sum(BigInt)`
// TODO: make this a type, but it's unclear how that should work
// See: <https://github.com/diesel-rs/diesel/discussions/4684>
pub type BigIntSum = BigDecimal;

/// Impractical conversion required for `diesel-rs` `sum(BigInt)` results.
pub fn sql_sum_into<T, E>(
    sum: &BigIntSum,
    table: &'static str,
    column: &'static str,
) -> Result<T, DatabaseError>
where
    E: std::error::Error + Send + Sync + 'static,
    T: TryFrom<bigdecimal::num_bigint::BigInt, Error = E>,
{
    let (val, exponent) = sum.as_bigint_and_exponent();
    debug_assert_eq!(
        exponent, 0,
        "We only sum(integers), hence there must never be a decimal result"
    );
    let val = T::try_from(val).map_err(|e| DatabaseError::ColumnSumExceedsLimit {
        table,
        column,
        limit: "<T>::MAX",
        source: Box::new(e),
    })?;
    Ok::<_, DatabaseError>(val)
}

#[derive(Debug, Clone, PartialEq, Insertable)]
#[diesel(table_name = notes)]
pub struct NoteInsertRowRaw {
    pub block_num: i64,

    pub batch_index: i32,
    pub note_index: i32, // index within batch

    pub note_id: Vec<u8>,

    pub note_type: i32,
    pub sender: Vec<u8>, // AccountId
    pub tag: i32,
    pub aux: i64,
    pub execution_hint: i64,

    pub consumed: i32,
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
            block_num: note.block_num.to_raw_sql(),
            batch_index: idx_to_raw_sql(note.note_index.batch_idx()),
            note_index: idx_to_raw_sql(note.note_index.note_idx_in_batch()),
            note_id: note.note_id.to_bytes(),
            note_type: note_type_to_raw_sql(note.metadata.note_type() as u8),
            sender: note.metadata.sender().to_bytes(),
            tag: note.metadata.tag().to_raw_sql(),
            execution_mode: execution_mode_to_raw_sql(note.metadata.tag().execution_mode() as i32),
            aux: aux_to_raw_sql(note.metadata.aux()),
            execution_hint: execution_hint_to_raw_sql(note.metadata.execution_hint().into()),
            inclusion_path: note.inclusion_path.to_bytes(),
            consumed: consumed_to_raw_sql(false), // New notes are always unconsumed.
            nullifier: nullifier.as_ref().map(Nullifier::to_bytes), /* Beware: `Option<T>` also implements `to_bytes`, but this is not what you want. */
            assets: note.details.as_ref().map(|d| d.assets().to_bytes()),
            inputs: note.details.as_ref().map(|d| d.inputs().to_bytes()),
            script_root: note.details.as_ref().map(|d| d.script().root().to_bytes()),
            serial_num: note.details.as_ref().map(|d| d.serial_num().to_bytes()),
        }
    }
}
