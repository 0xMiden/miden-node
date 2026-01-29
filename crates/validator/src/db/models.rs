use diesel::prelude::*;
use miden_protocol::transaction::TransactionHeader;
use miden_protocol::utils::Serializable;

use crate::db::schema;

#[derive(Debug, Clone, PartialEq, Insertable)]
#[diesel(table_name = schema::transactions)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct TransactionSummaryRowInsert {
    transaction_id: Vec<u8>,
    account_id: Vec<u8>,
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
    pub fn new(transaction_header: &TransactionHeader) -> Self {
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
            initial_state_commitment: transaction_header.initial_state_commitment().to_bytes(),
            final_state_commitment: transaction_header.final_state_commitment().to_bytes(),
            input_notes: input_notes_binary,
            output_notes: output_notes_binary,
            size_in_bytes,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Queryable, Selectable)]
#[diesel(table_name = schema::transactions)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct TransactionSummaryRowSelect {
    pub transaction_id: Vec<u8>,
    pub account_id: Vec<u8>,
    pub initial_state_commitment: Vec<u8>,
    pub final_state_commitment: Vec<u8>,
    pub input_notes: Vec<u8>,
    pub output_notes: Vec<u8>,
    pub size_in_bytes: i64,
}
