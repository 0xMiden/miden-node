use miden_objects::{
    Digest,
    account::AccountId,
    block::{AccountTree, BlockHeader},
    note::NoteDetails,
    utils::{Deserializable, Serializable},
};
use rusqlite::{Connection, params};

pub fn migrate_release_0_9(conn: &mut Connection) -> anyhow::Result<()> {
    migrate_note_details(conn)?;
    migrate_account_root(conn)?;
    Ok(())
}

/// Queries the latest header, recomputes its account root and updates it in the db. This is needed
/// because the way the account root is computed changed in v0.9.
///
/// Note: this is idempotent.
pub fn migrate_account_root(conn: &mut Connection) -> anyhow::Result<()> {
    // get the latest block header (code taken from
    // `miden_node_store::sql::select_block_header_by_block_num`)
    let transaction = conn.transaction().unwrap();
    let latest_header = {
        let mut stmt = transaction.prepare_cached(
            "SELECT block_header FROM block_headers ORDER BY block_num DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;

        let Some(data) = rows.next()? else {
            // if there is no header, the db is empty and we can return early
            return Ok(());
        };
        let data = data.get_ref(0)?.as_blob()?;

        BlockHeader::read_from_bytes(data)?
    };

    // get the account commitments from the store (code taken from
    // `miden_node_store::sql::select_all_account_commitments`) and build the account tree to get
    // the root
    let new_account_root = {
        let mut stmt = transaction.prepare_cached(
            "SELECT account_id, account_commitment FROM accounts ORDER BY block_num ASC;",
        )?;
        let mut rows = stmt.query([])?;

        let mut entries = Vec::new();
        while let Some(row) = rows.next()? {
            let account_id_data = row.get_ref(0)?.as_blob()?;
            let account_id = AccountId::read_from_bytes(account_id_data)?;

            let account_commitment_data = row.get_ref(1)?.as_blob()?;
            let account_commitment = Digest::read_from_bytes(account_commitment_data)?;

            entries.push((account_id, account_commitment));
        }

        AccountTree::with_entries(entries)?.root()
    };

    // create a new header with same fields but different account root
    let new_header = BlockHeader::new(
        latest_header.version(),
        latest_header.prev_block_commitment(),
        latest_header.block_num(),
        latest_header.chain_commitment(),
        new_account_root,
        latest_header.nullifier_root(),
        latest_header.note_root(),
        latest_header.tx_commitment(),
        latest_header.tx_kernel_commitment(),
        latest_header.proof_commitment(),
        latest_header.timestamp(),
    );

    assert_eq!(new_header.block_num(), latest_header.block_num());

    // update the header in the db
    let affected_rows = {
        let mut stmt = transaction
            .prepare_cached("UPDATE block_headers SET block_header = ? WHERE block_num = ?;")?;
        stmt.execute(params![new_header.to_bytes(), latest_header.block_num().as_u64()])?
    };

    assert_eq!(affected_rows, 1);

    transaction.commit()?;
    Ok(())
}

/// Deserialize the note details from the db and store them in separate columns. This is needed
/// because the way the note details are stored in the db changed in v0.9.
pub fn migrate_note_details(conn: &mut Connection) -> anyhow::Result<()> {
    let transaction = conn.transaction().unwrap();
    // in the sql migration the details are migrated to the assets column, so we read them from
    // there
    let mut stmt = transaction.prepare_cached(
        "SELECT note_id, details
        FROM notes_new
        WHERE details IS NOT NULL
        AND assets IS NULL
        AND inputs IS NULL
        AND script_root IS NULL
        AND serial_num IS NULL",
    )?;
    let mut rows = stmt.query([])?;

    while let Some(row) = rows.next()? {
        let note_id = row.get_ref(0)?.as_blob()?;
        let note_details = row.get_ref(1)?.as_blob()?;

        let note_details = NoteDetails::read_from_bytes(note_details)?;

        let note_assets = note_details.assets().to_bytes();
        let note_inputs = note_details.inputs().to_bytes();
        let note_script_root = note_details.script().root().to_bytes();
        let note_serial_num = note_details.serial_num().to_bytes();

        // add the script to the note_scripts table
        let mut stmt = transaction
            .prepare_cached("INSERT INTO note_scripts (script_root, script) VALUES (?, ?)")?;
        stmt.execute(params![note_script_root, note_details.script().to_bytes()])?;

        // add the details to the notes table
        let mut stmt = transaction.prepare_cached(
            "UPDATE notes_new SET assets = ?, inputs = ?, script_root = ?, serial_num = ? WHERE note_id = ?",
        )?;
        stmt.execute(params![
            note_assets,
            note_inputs,
            note_script_root,
            note_serial_num,
            note_id
        ])?;
    }

    // drop the old table and rename the new one
    let mut stmt = transaction.prepare_cached("DROP TABLE notes")?;
    stmt.execute([])?;
    let mut stmt = transaction.prepare_cached("ALTER TABLE notes_new RENAME TO notes")?;
    stmt.execute([])?;
    let mut stmt = transaction.prepare_cached("ALTER TABLE notes_new DROP COLUMN details")?;
    stmt.execute([])?;

    Ok(())
}
