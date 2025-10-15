use core::any::type_name;

use diesel::prelude::*;
use miden_node_store::{DatabaseError, DatabaseTypeConversionError};
use miden_objects::Word;
use miden_objects::account::AccountId;
use miden_objects::block::BlockNumber;
use miden_objects::note::{NoteId, Nullifier};
use miden_objects::transaction::TransactionId;
use miden_objects::utils::{Deserializable, Serializable};

use crate::db::schema;

#[derive(Debug, PartialEq)]
pub struct TransactionSummary {
    pub account_id: AccountId,
    pub block_num: BlockNumber,
    pub transaction_id: TransactionId,
}

#[derive(Debug, Clone, PartialEq, Queryable, Selectable, QueryableByName)]
#[diesel(table_name = schema::transactions)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct TransactionSummaryRaw {
    account_id: Vec<u8>,
    block_num: i64,
    transaction_id: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Queryable, Selectable, QueryableByName)]
#[diesel(table_name = schema::transactions)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct TransactionRecordRaw {
    account_id: Vec<u8>,
    block_num: i64,
    transaction_id: Vec<u8>,
    initial_state_commitment: Vec<u8>,
    final_state_commitment: Vec<u8>,
    input_notes: Vec<u8>,
    output_notes: Vec<u8>,
    size_in_bytes: i64,
}

impl TryInto<TransactionSummary> for TransactionSummaryRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<TransactionSummary, Self::Error> {
        Ok(TransactionSummary {
            account_id: AccountId::read_from_bytes(&self.account_id[..])?,
            block_num: BlockNumber::from_raw_sql(self.block_num)?,
            transaction_id: TransactionId::read_from_bytes(&self.transaction_id[..])?,
        })
    }
}

#[derive(Debug, PartialEq)]
pub struct TransactionRecord {
    pub block_num: BlockNumber,
    pub transaction_id: TransactionId,
    pub account_id: AccountId,
    pub initial_state_commitment: Word,
    pub final_state_commitment: Word,
    pub input_notes: Vec<Nullifier>, // Store nullifiers for input notes
    pub output_notes: Vec<NoteId>,   // Store note IDs for output notes
}

impl TryInto<TransactionRecord> for TransactionRecordRaw {
    type Error = DatabaseError;
    fn try_into(self) -> Result<TransactionRecord, Self::Error> {
        use miden_objects::Word;

        let initial_state_commitment = self.initial_state_commitment;
        let final_state_commitment = self.final_state_commitment;
        let input_notes_binary = self.input_notes;
        let output_notes_binary = self.output_notes;

        // Deserialize input notes as nullifiers and output notes as note IDs
        let input_notes: Vec<Nullifier> = Deserializable::read_from_bytes(&input_notes_binary)?;
        let output_notes: Vec<NoteId> = Deserializable::read_from_bytes(&output_notes_binary)?;

        Ok(TransactionRecord {
            account_id: AccountId::read_from_bytes(&self.account_id[..])?,
            block_num: BlockNumber::from_raw_sql(self.block_num)?,
            transaction_id: TransactionId::read_from_bytes(&self.transaction_id[..])?,
            initial_state_commitment: Word::read_from_bytes(&initial_state_commitment)?,
            final_state_commitment: Word::read_from_bytes(&final_state_commitment)?,
            input_notes,
            output_notes,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Insertable)]
#[diesel(table_name = schema::transactions)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct TransactionSummaryRowInsert {
    transaction_id: Vec<u8>,
    account_id: Vec<u8>,
    block_num: i64,
    initial_state_commitment: Vec<u8>,
    final_state_commitment: Vec<u8>,
    input_notes: Vec<u8>,
    output_notes: Vec<u8>,
    size_in_bytes: i64,
}

impl TransactionSummaryRowInsert {
    #[allow(
        clippy::cast_possible_wrap,
        reason = "We will not approach the item count where i64 and usize cause issues"
    )]
    fn new(
        transaction_header: &miden_objects::transaction::TransactionHeader,
        block_num: BlockNumber,
    ) -> Self {
        const HEADER_BASE_SIZE: usize = 4 + 32 + 16 + 64; // block_num + tx_id + account_id + commitments

        // Serialize input notes using binary format (store nullifiers)
        let input_notes_binary = transaction_header.input_notes().to_bytes();

        // Serialize output notes using binary format (store note IDs)
        let output_notes_binary = transaction_header.output_notes().to_bytes();

        // Manually calculate the estimated size of the transaction header to avoid
        // the cost of serialization. The size estimation includes:
        // - 4 bytes for block number
        // - 32 bytes for transaction ID
        // - 16 bytes for account ID
        // - 64 bytes for initial + final state commitments (32 bytes each)
        // - 32 bytes per input note (nullifier size)
        // - 500 bytes per output note (estimated size when converted to NoteSyncRecord)
        //
        // Note: 500 bytes per output note is an over-estimate but ensures we don't
        // exceed memory limits when these transactions are later converted to proto records.
        let input_notes_size = (transaction_header.input_notes().num_notes() * 32) as usize;
        let output_notes_size = transaction_header.output_notes().len() * 500;
        let size_in_bytes = (HEADER_BASE_SIZE + input_notes_size + output_notes_size) as i64;

        Self {
            transaction_id: transaction_header.id().to_bytes(),
            account_id: transaction_header.account_id().to_bytes(),
            block_num: block_num.to_raw_sql(),
            initial_state_commitment: transaction_header.initial_state_commitment().to_bytes(),
            final_state_commitment: transaction_header.final_state_commitment().to_bytes(),
            input_notes: input_notes_binary,
            output_notes: output_notes_binary,
            size_in_bytes,
        }
    }
}

/// Convert from and to it's database representation and back
///
/// We do not assume sanity of DB types.
pub(crate) trait SqlTypeConvert: Sized {
    type Raw: Sized;
    type Error: std::error::Error + Send + Sync + 'static;
    fn to_raw_sql(self) -> Self::Raw;
    fn from_raw_sql(_raw: Self::Raw) -> Result<Self, Self::Error>;
}

impl SqlTypeConvert for BlockNumber {
    type Raw = i64;
    type Error = DatabaseTypeConversionError;
    fn from_raw_sql(raw: Self::Raw) -> Result<Self, Self::Error> {
        u32::try_from(raw)
            .map(BlockNumber::from)
            .map_err(|_| DatabaseTypeConversionError(type_name::<BlockNumber>()))
    }
    fn to_raw_sql(self) -> Self::Raw {
        i64::from(self.as_u32())
    }
}
