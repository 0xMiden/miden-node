//! Row mapping shared by the `notes` queries.
//!
//! The `notes` queries all select their columns in one of two fixed orders, so a single mapper can
//! serve them: the sync-record order (see [`note_sync_record_from_row`]) and the full-record order
//! (see [`note_record_from_row`]), which extends it with the detail columns and the joined script.

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::Row;
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::block::{BlockNoteIndex, BlockNumber};
use miden_protocol::crypto::merkle::SparseMerklePath;
use miden_protocol::note::{
    NoteAssets,
    NoteAttachments,
    NoteDetails,
    NoteId,
    NoteMetadata,
    NoteRecipient,
    NoteScript,
    NoteStorage,
    NoteTag,
    NoteType,
    PartialNoteMetadata,
};

use crate::db::{NoteRecord, NoteSyncRecord};

/// Maps a row selecting `committed_at, batch_index, note_index, note_id, note_type, sender, tag,
/// attachment, inclusion_path` to a [`NoteSyncRecord`].
pub(super) fn note_sync_record_from_row(row: &Row<'_>) -> Result<NoteSyncRecord, DatabaseError> {
    let (metadata, attachments) = note_metadata_from_row(row, 4)?;

    Ok(NoteSyncRecord {
        block_num: row.get::<BlockNumber>(0)?,
        note_index: block_note_index_from_row(row, 1)?,
        note_id: NoteId::from_raw(row.get::<Word>(3)?),
        metadata,
        attachments,
        inclusion_path: row.get::<SparseMerklePath>(8)?,
    })
}

/// Maps a row selecting `committed_at, batch_index, note_index, note_id, note_type, sender, tag,
/// attachment, assets, storage, serial_num, inclusion_path, script` to a [`NoteRecord`].
///
/// The script column comes from the left join on `note_scripts` and is therefore nullable, as are
/// the detail columns; a note carries details only when all of them are present.
pub(super) fn note_record_from_row(row: &Row<'_>) -> Result<NoteRecord, DatabaseError> {
    let (metadata, attachments) = note_metadata_from_row(row, 4)?;
    let details = note_details_from_row(row, 8)?;

    Ok(NoteRecord {
        block_num: row.get::<BlockNumber>(0)?,
        note_index: block_note_index_from_row(row, 1)?,
        note_id: row.get::<Word>(3)?,
        metadata,
        details,
        attachments,
        inclusion_path: row.get::<SparseMerklePath>(11)?,
    })
}

/// Maps `note_type, sender, tag, attachment` starting at `offset` to a note's metadata.
fn note_metadata_from_row(
    row: &Row<'_>,
    offset: usize,
) -> Result<(NoteMetadata, NoteAttachments), DatabaseError> {
    let note_type = NoteType::try_from(row.get::<u8>(offset)?)
        .map_err(|err| DatabaseError::deserialization("NoteType", err))?;
    let sender = row.get::<AccountId>(offset + 1)?;
    let tag = row.get::<NoteTag>(offset + 2)?;

    // An empty blob means the note has no attachments, rather than being a serialized empty value.
    let attachment = row.get::<Vec<u8>>(offset + 3)?;
    let attachments = if attachment.is_empty() {
        NoteAttachments::empty()
    } else {
        row.get::<NoteAttachments>(offset + 3)?
    };

    let partial = PartialNoteMetadata::new(sender, note_type).with_tag(tag);
    Ok((NoteMetadata::new(partial, &attachments), attachments))
}

/// Maps `batch_index, note_index` starting at `offset` to a [`BlockNoteIndex`].
fn block_note_index_from_row(
    row: &Row<'_>,
    offset: usize,
) -> Result<BlockNoteIndex, DatabaseError> {
    let batch_index = row.get::<u32>(offset)? as usize;
    let note_index = row.get::<u32>(offset + 1)? as usize;

    BlockNoteIndex::new(batch_index, note_index).ok_or_else(|| {
        DatabaseError::conversiont_from_sql::<BlockNoteIndex, DatabaseError, _>(None)
    })
}

/// Maps `assets, storage, serial_num` starting at `offset`, plus the joined `script` column that
/// follows them, to a note's details.
///
/// Private notes store none of these, in which case there are no details to reconstruct.
fn note_details_from_row(
    row: &Row<'_>,
    offset: usize,
) -> Result<Option<NoteDetails>, DatabaseError> {
    let assets = row.get::<Option<NoteAssets>>(offset)?;
    let storage = row.get::<Option<NoteStorage>>(offset + 1)?;
    let serial_num = row.get::<Option<Word>>(offset + 2)?;
    // The script sits after the `inclusion_path` column, which the details do not use.
    let script = row.get::<Option<NoteScript>>(offset + 4)?;

    let (Some(assets), Some(storage), Some(serial_num)) = (assets, storage, serial_num) else {
        return Ok(None);
    };
    // A note with details must have a script; the join failing to find one means the note's script
    // was never stored.
    let script = script.ok_or_else(|| {
        DatabaseError::conversiont_from_sql::<NoteRecipient, DatabaseError, _>(None)
    })?;

    let recipient = NoteRecipient::new(serial_num, script, storage);
    Ok(Some(NoteDetails::new(assets, recipient)))
}
