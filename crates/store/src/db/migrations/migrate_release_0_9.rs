use std::sync::Arc;

use miden_core::{Felt, Word, mast::serialization::read_from_old};
use miden_lib::utils::{ByteReader, SliceReader};
use miden_objects::{
    Digest, MastForest, MastNodeId,
    account::{Account, AccountCode, AccountId, AccountProcedureInfo, AccountStorage},
    asset::{Asset, AssetVault, FungibleAsset},
    block::{AccountTree, BlockHeader},
    note::{Note, NoteAssets, NoteInputs, NoteRecipient, NoteScript},
    utils::{Deserializable, Serializable},
};
use rusqlite::{Connection, params};

pub fn migrate_release_0_9(conn: &mut Connection) -> anyhow::Result<()> {
    migrate_note_details(conn)?;
    migrate_account_codes(conn)?;
    migrate_account_root(conn)?;
    Ok(())
}

fn migrate_account_codes(conn: &mut Connection) -> anyhow::Result<()> {
    let transaction = conn.transaction().unwrap();
    {
        let mut stmt =
            transaction.prepare_cached("SELECT details FROM accounts WHERE details IS NOT NULL")?;
        let mut rows = stmt.query([])?;

        while let Some(data) = rows.next()? {
            let public_details = data.get_ref(0)?.as_blob()?;
            let mut reader = SliceReader::new(public_details);
            let id = AccountId::read_from(&mut reader)?;
            let vault = AssetVault::read_from(&mut reader)?;
            let storage = AccountStorage::read_from(&mut reader)?;

            let mut buffer: Vec<u8> = vec![];
            while reader.has_more_bytes() {
                buffer.push(reader.read_u8()?);
            }
            let mut reader_for_0_8 = SliceReader::new(&buffer);
            let mut reader = SliceReader::new(&buffer);

            let module = if let Err(_) = MastForest::read_from(&mut reader_for_0_8) {
                // try old version
                read_from_old(&mut reader)?
            } else {
                // was already converted
                continue;
            };
            let module = Arc::new(MastForest::read_from_bytes(&module.to_bytes())?);
            let num_procedures = (reader.read_u8()? as usize) + 1;
            let procedures = reader.read_many::<AccountProcedureInfo>(num_procedures)?;

            let code = AccountCode::from_parts(module, procedures);

            let nonce = Felt::read_from(&mut reader)?;

            let account = Account::from_parts(id, vault, storage, code, nonce);

            let mut stmt =
                transaction.prepare_cached("UPDATE accounts SET details = ? WHERE account_id=?")?;

            let result =
                stmt.execute(params![Some(account.to_bytes()), account.id().to_bytes()])?;
            println!("updated {result} account (id {})", account.id());
        }
    }

    transaction.commit()?;

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
    let transaction = conn.transaction()?;
    {
        // in the sql migration the details are migrated to the assets column, so we read them from
        // there
        let mut stmt = transaction.prepare_cached(
            "SELECT note_id, assets
            FROM notes
            WHERE inputs IS NULL
            AND script_root IS NULL
            AND serial_num IS NULL 
            AND assets IS NOT NULL
            AND note_type = 1
            ",
        )?;
        let mut rows = stmt.query([])?;

        while let Some(row) = rows.next()? {
            let note_id = row.get_ref(0)?.as_blob()?;
            let note_details = row
                .get_ref(1)?
                .as_blob_or_null()?
                .ok_or_else(|| anyhow::anyhow!("unexpected NULL note_details"))?;
            let note_details = <Vec<u8>>::read_from_bytes(note_details)?;

            let mut byte_reader = SliceReader::new(&note_details);

            let note_metadata = miden_objects::note::NoteMetadata::read_from(&mut byte_reader)?;
            let assets_count = byte_reader.read_u8()?;

            let mut assets = Vec::new();
            for _ in 0..assets_count {
                let a = FungibleAsset::read_from(&mut byte_reader)?;
                assets.push(Asset::Fungible(a));
            }

            let note_assets = NoteAssets::new(assets)?;

            let note_mast_forest = read_from_old(&mut byte_reader)?;
            let note_mast_serialized = note_mast_forest.to_bytes();
            let new_mast_forest = MastForest::read_from_bytes(&note_mast_serialized)?;

            let entrypoint = MastNodeId::from_u32_safe(byte_reader.read_u32()?, &new_mast_forest)?;

            let note_script = NoteScript::from_parts(Arc::new(new_mast_forest), entrypoint);

            let inputs = NoteInputs::read_from(&mut byte_reader)?;
            let serial_num = Word::read_from(&mut byte_reader)?;
            let recipient = NoteRecipient::new(serial_num, note_script.clone(), inputs);

            let note = Note::new(note_assets, note_metadata, recipient);

            let note_assets = note.assets().to_bytes();
            let note_inputs = note.inputs().to_bytes();
            let note_script_root = note.script().root().to_bytes();
            let note_serial_num = note.serial_num().to_bytes();

            // add the script to the note_scripts table
            let mut stmt = transaction.prepare_cached(
                "INSERT OR REPLACE INTO note_scripts (script_root, script) VALUES (?, ?)",
            )?;
            stmt.execute(params![note_script_root, note_script.to_bytes()])?;

            // add the details to the notes table
            let mut stmt = transaction.prepare_cached(
            "UPDATE notes SET assets = ?, inputs = ?, script_root = ?, serial_num = ? WHERE note_id = ?",
        )?;

            stmt.execute(params![
                note_assets,
                note_inputs,
                note_script_root,
                note_serial_num,
                note_id
            ])?;
        }
    }

    transaction.commit()?;

    Ok(())
}
