mod migrations;
mod models;
mod schema;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use diesel::SqliteConnection;
use diesel::prelude::*;
use miden_node_store::{ConnectionManager, DatabaseError, DatabaseSetupError};
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::asset::FungibleAsset;
use miden_protocol::block::BlockNumber;
use miden_protocol::note::{NoteHeader, NoteMetadata, NoteTag, NoteType};
use miden_protocol::transaction::{
    InputNotes,
    OrderedTransactionHeaders,
    OutputNote,
    OutputNotes,
    TransactionHeader,
    TransactionId,
};
use miden_protocol::utils::{Deserializable, Serializable};
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

pub(crate) fn insert_transaction(
    conn: &mut SqliteConnection,
    header: TransactionHeader,
) -> Result<usize, DatabaseError> {
    #[allow(clippy::into_iter_on_ref)] // false positive
    let row = TransactionSummaryRowInsert::new(&header);
    let count = diesel::insert_into(schema::transactions::table).values(row).execute(conn)?;
    Ok(count)
}

pub(crate) fn select_transactions(
    conn: &mut SqliteConnection,
    tx_ids: &[TransactionId],
) -> Result<HashMap<TransactionId, TransactionHeader>, DatabaseError> {
    if tx_ids.is_empty() {
        return Ok(HashMap::new());
    }

    // Convert TransactionIds to bytes for query
    let tx_id_bytes: Vec<Vec<u8>> = tx_ids.iter().map(TransactionId::to_bytes).collect();

    // Query the database for matching transactions
    let raw_transactions = schema::transactions::table
        .filter(schema::transactions::transaction_id.eq_any(tx_id_bytes))
        .order(schema::transactions::transaction_id.asc())
        .load::<TransactionSummaryRowSelect>(conn)
        .map_err(DatabaseError::from)?;

    let mut transactions: HashMap<TransactionId, TransactionHeader> = HashMap::new();

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

        // TODO(sergerad): Implement into_iter for OutputNotes to avoid clones.
        let output_note_headers = output_notes
            .iter()
            .map(|note| match note {
                OutputNote::Full(full_note) => full_note.header().clone(),
                OutputNote::Header(header) => header.clone(),
                OutputNote::Partial(partial) => {
                    // For partial notes, create a minimal header with the note ID.
                    // Using default values for metadata since we don't have full note data.
                    let metadata = NoteMetadata::new(account_id, NoteType::Public, NoteTag::new(0));
                    NoteHeader::new(partial.id(), metadata)
                },
            })
            .collect::<Vec<_>>();

        // Create TransactionHeader
        let tx_header = TransactionHeader::new_unchecked(
            tx_id,
            account_id,
            initial_state_commitment,
            final_state_commitment,
            input_notes,
            output_note_headers,
            FungibleAsset::new(account_id, 0).expect("todo"), // todo(currentpr): ?
        );

        transactions.insert(tx_id, tx_header);
    }

    Ok(transactions)
}
