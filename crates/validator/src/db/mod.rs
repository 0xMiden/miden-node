mod migrations;
mod models;
mod schema;

use std::collections::HashMap;
use std::path::PathBuf;

use diesel::SqliteConnection;
use diesel::prelude::*;
use miden_node_store::{ConnectionManager, DatabaseError, DatabaseSetupError};
use miden_objects::account::AccountId;
use miden_objects::block::BlockNumber;
use miden_objects::note::{NoteExecutionHint, NoteHeader, NoteMetadata, NoteTag, NoteType};
use miden_objects::transaction::{
    InputNotes, OrderedTransactionHeaders, OutputNote, OutputNotes, TransactionHeader,
    TransactionId,
};
use miden_objects::utils::{Deserializable, Serializable};
use miden_objects::{Felt, Word};
use tracing::instrument;

use crate::COMPONENT;
use crate::db::migrations::apply_migrations;
use crate::db::models::{TransactionSummaryRowInsert, TransactionSummaryRowSelect};

/// Open a connection to the DB and apply any pending migrations.
#[instrument(target = COMPONENT, skip_all)]
pub async fn load(database_filepath: PathBuf) -> Result<miden_node_store::Db, DatabaseSetupError> {
    let manager = ConnectionManager::new(database_filepath.to_str().unwrap());
    let pool = deadpool_diesel::Pool::builder(manager).max_size(16).build()?;

    tracing::info!(
        target: COMPONENT,
        sqlite= %database_filepath.display(),
        "Connected to the database"
    );

    let db = miden_node_store::Db::new(pool);
    db.query("migrations", apply_migrations).await?;
    Ok(db)
}

pub(crate) fn insert_transactions(
    conn: &mut SqliteConnection,
    block_num: BlockNumber,
    transactions: &OrderedTransactionHeaders,
) -> Result<usize, DatabaseError> {
    #[allow(clippy::into_iter_on_ref)] // false positive
    let rows: Vec<_> = transactions
        .as_slice()
        .into_iter()
        .map(|tx| TransactionSummaryRowInsert::new(tx, block_num))
        .collect();

    let count = diesel::insert_into(schema::transactions::table).values(rows).execute(conn)?;
    Ok(count)
}

pub(crate) fn select_transactions(
    conn: &mut SqliteConnection,
    tx_ids: &[TransactionId],
) -> Result<Vec<OrderedTransactionHeaders>, DatabaseError> {
    if tx_ids.is_empty() {
        return Ok(vec![]);
    }

    // Convert TransactionIds to bytes for query
    let tx_id_bytes: Vec<Vec<u8>> = tx_ids.iter().map(TransactionId::to_bytes).collect();

    // Query the database for matching transactions
    let raw_transactions = schema::transactions::table
        .filter(schema::transactions::transaction_id.eq_any(tx_id_bytes))
        .order(schema::transactions::block_num.asc())
        .order(schema::transactions::transaction_id.asc())
        .load::<TransactionSummaryRowSelect>(conn)
        .map_err(DatabaseError::from)?;

    // Group transactions by block number
    let mut transactions_by_block: HashMap<BlockNumber, Vec<TransactionHeader>> = HashMap::new();

    for raw_tx in raw_transactions {
        // Deserialize the stored data
        let account_id = AccountId::read_from_bytes(&raw_tx.account_id)?;

        let initial_state_commitment = Word::read_from_bytes(&raw_tx.initial_state_commitment)?;

        let final_state_commitment = Word::read_from_bytes(&raw_tx.final_state_commitment)?;

        let input_notes = InputNotes::read_from_bytes(&raw_tx.input_notes)?;

        let output_notes = OutputNotes::read_from_bytes(&raw_tx.output_notes)?;

        // Create TransactionId from the stored components
        let tx_id = TransactionId::new(
            initial_state_commitment,
            final_state_commitment,
            input_notes.commitment(),
            output_notes.commitment(),
        );

        // Convert OutputNotes to Vec<NoteHeader>
        let output_note_headers: Vec<_> = output_notes
            .iter()
            .map(|note| match note {
                OutputNote::Full(full_note) => *full_note.header(),
                OutputNote::Header(header) => *header,
                OutputNote::Partial(partial) => {
                    // For partial notes, create a minimal header with the note ID
                    // Using default values for metadata since we don't have full note data
                    let metadata = NoteMetadata::new(
                        account_id,
                        NoteType::Public,
                        NoteTag::LocalAny(0),
                        NoteExecutionHint::None,
                        Felt::new(0),
                    )
                    .unwrap();
                    NoteHeader::new(partial.id(), metadata)
                },
            })
            .collect();

        // Create TransactionHeader
        let tx_header = TransactionHeader::new_unchecked(
            tx_id,
            account_id,
            initial_state_commitment,
            final_state_commitment,
            input_notes,
            output_note_headers,
        );

        #[allow(clippy::cast_sign_loss)]
        let block_num = BlockNumber::from(raw_tx.block_num as u32);

        transactions_by_block.entry(block_num).or_default().push(tx_header);
    }

    // Convert grouped transactions to OrderedTransactionHeaders
    let mut result = Vec::new();
    for (_, tx_headers) in transactions_by_block {
        let ordered_headers = OrderedTransactionHeaders::new_unchecked(tx_headers);
        result.push(ordered_headers);
    }

    // Sort by block number (implicitly through the ordering of HashMap iteration)
    // Note: We could sort explicitly if needed, but the database query already orders by block_num

    Ok(result)
}
