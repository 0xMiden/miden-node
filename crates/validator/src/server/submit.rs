use diesel::SqliteConnection;
use miden_node_store::DatabaseError;
use miden_objects::transaction::{OrderedTransactionHeaders, ProvenTransaction, TransactionHeader};
use tracing::{debug, instrument};

use crate::COMPONENT;
use crate::db::insert_transactions;

#[instrument(
        target = COMPONENT,
        name = "validator.submit_proven_transaction",
        skip(conn, proven_tx),
    )]
pub fn submit_proven_transaction(
    conn: &mut SqliteConnection,
    proven_tx: &ProvenTransaction,
) -> Result<usize, DatabaseError> {
    debug!(target: COMPONENT, "Starting proven transaction submission");

    let tx_id = proven_tx.id();
    let account_id = proven_tx.account_id();

    debug!(
        target = COMPONENT,
        tx_id = %tx_id.to_hex(),
        account_id = %account_id.to_hex(),
        initial_state_commitment = %proven_tx.account_update().initial_state_commitment(),
        final_state_commitment = %proven_tx.account_update().final_state_commitment(),
        "deserialized proven transaction"
    );

    // Create transaction header from the proven transaction
    let transaction_header = TransactionHeader::from(proven_tx);

    // Create a single-item collection for the insert_transactions function
    let ordered_transactions = OrderedTransactionHeaders::new_unchecked(vec![transaction_header]);

    // Insert the transaction into the database
    let rows_affected =
        insert_transactions(conn, proven_tx.ref_block_num(), &ordered_transactions)?;

    debug!(
        target = COMPONENT,
        rows_affected = rows_affected,
        "Successfully inserted proven transaction into database"
    );

    Ok(rows_affected)
}
