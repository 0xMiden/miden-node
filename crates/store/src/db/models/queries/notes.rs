#![allow(
    clippy::cast_possible_wrap,
    reason = "We will not approach the item count where i64 and usize cause issues"
)]

use std::collections::{BTreeMap, BTreeSet};

use diesel::prelude::{
    BoolExpressionMethods,
    ExpressionMethods,
    Queryable,
    QueryableByName,
    Selectable,
    SelectableHelper,
};
use diesel::query_dsl::methods::SelectDsl;
use diesel::query_dsl::{QueryDsl, RunQueryDsl};
use diesel::sqlite::Sqlite;
use diesel::{JoinOnDsl, NullableExpressionMethods, OptionalExtension, SqliteConnection};
use miden_lib::utils::Deserializable;
use miden_node_utils::limiter::{
    QueryParamAccountIdLimit,
    QueryParamLimiter,
    QueryParamNoteIdLimit,
    QueryParamNoteTagLimit,
};
use miden_objects::account::AccountId;
use miden_objects::block::{BlockNoteIndex, BlockNumber};
use miden_objects::crypto::merkle::SparseMerklePath;
use miden_objects::note::{
    NoteAssets,
    NoteDetails,
    NoteExecutionHint,
    NoteExecutionMode,
    NoteId,
    NoteInclusionProof,
    NoteInputs,
    NoteMetadata,
    NoteRecipient,
    NoteScript,
    NoteTag,
    NoteType,
};
use miden_objects::{Felt, Word};

use super::DatabaseError;
use crate::db::models::conv::{SqlTypeConvert, raw_sql_to_idx};
use crate::db::models::queries::select_block_header_by_block_num;
use crate::db::models::{serialize_vec, vec_raw_try_into};
use crate::db::{NoteRecord, NoteSyncRecord, NoteSyncUpdate, Page, schema};
use crate::errors::NoteSyncError;

/// Select notes matching the tags and account IDs search criteria using the given
/// [`SqliteConnection`].
///
/// # Returns
///
/// All matching notes from the first block greater than `block_num` containing a matching note.
/// A note is considered a match if it has any of the given tags, or if its sender is one of the
/// given account IDs. If no matching notes are found at all, then an empty vector is returned.
///
/// # Note
///
/// This method returns notes from a single block. To fetch all notes up to the chain tip,
/// multiple requests are necessary.
pub(crate) fn select_notes_since_block_by_tag_and_sender(
    conn: &mut SqliteConnection,
    start_block_number: BlockNumber,
    account_ids: &[AccountId],
    note_tags: &[u32],
) -> Result<Vec<NoteSyncRecord>, DatabaseError> {
    QueryParamAccountIdLimit::check(account_ids.len())?;
    QueryParamNoteTagLimit::check(note_tags.len())?;
    // SELECT
    //     block_num,
    //     batch_index,
    //     note_index,
    //     note_id,
    //     note_type,
    //     sender,
    //     tag,
    //     aux,
    //     execution_hint,
    //     inclusion_path
    // FROM
    //     notes
    // WHERE
    //     -- find the next block which contains at least one note with a matching tag or sender
    //     block_num = (
    //         SELECT
    //             block_num
    //         FROM
    //             notes
    //         WHERE
    //             (tag IN rarray(?1) OR sender IN rarray(?2)) AND
    //             block_num > ?3
    //         ORDER BY
    //             block_num ASC
    //     LIMIT 1) AND
    //     -- filter the block's notes and return only the ones matching the requested tags or
    // senders     (tag IN rarray(?1) OR sender IN rarray(?2))

    let desired_note_tags = Vec::from_iter(note_tags.iter().map(|tag| *tag as i32));
    let desired_senders = serialize_vec(account_ids.iter());

    let start_block_num = start_block_number.to_raw_sql();

    // find block_num: select notes since block by tag and sender
    let Some(desired_block_num): Option<i64> =
        SelectDsl::select(schema::notes::table, schema::notes::committed_at)
            .filter(
                schema::notes::tag
                    .eq_any(&desired_note_tags[..])
                    .or(schema::notes::sender.eq_any(&desired_senders[..])),
            )
            .filter(schema::notes::committed_at.gt(start_block_num))
            .order_by(schema::notes::committed_at.asc())
            .limit(1)
            .get_result(conn)
            .optional()?
    else {
        return Ok(Vec::new());
    };

    let notes = SelectDsl::select(schema::notes::table, NoteSyncRecordRawRow::as_select())
            // find the next block which contains at least one note with a matching tag or sender
            .filter(schema::notes::committed_at.eq(
                &desired_block_num
            ))
            // filter the block's notes and return only the ones matching the requested tags or senders
            .filter(
                schema::notes::tag
                .eq_any(&desired_note_tags)
                .or(
                    schema::notes::sender
                    .eq_any(&desired_senders)
                )
            )
            .get_results::<NoteSyncRecordRawRow>(conn)
            .map_err(DatabaseError::from)?;

    vec_raw_try_into(notes)
}

pub(crate) fn select_notes_by_id(
    conn: &mut SqliteConnection,
    note_ids: &[NoteId],
) -> Result<Vec<NoteRecord>, DatabaseError> {
    let note_ids = serialize_vec(note_ids);
    // SELECT {}
    // FROM notes
    // LEFT JOIN note_scripts ON notes.script_root = note_scripts.script_root
    // WHERE note_id IN rarray(?1),

    let q = schema::notes::table
        .left_join(
            schema::note_scripts::table
                .on(schema::notes::script_root.eq(schema::note_scripts::script_root.nullable())),
        )
        .filter(schema::notes::note_id.eq_any(&note_ids));
    let raw: Vec<_> = SelectDsl::select(
        q,
        (NoteRecordRawRow::as_select(), schema::note_scripts::script.nullable()),
    )
    .load::<(NoteRecordRawRow, Option<Vec<u8>>)>(conn)?;
    let records = vec_raw_try_into::<NoteRecord, NoteRecordWithScriptRawJoined>(
        raw.into_iter().map(NoteRecordWithScriptRawJoined::from),
    )?;
    Ok(records)
}

/// Select all notes from the DB using the given [`SqliteConnection`].
///
///
/// # Returns
///
/// A vector with notes, or an error.
#[cfg(test)]
pub(crate) fn select_all_notes(
    conn: &mut SqliteConnection,
) -> Result<Vec<NoteRecord>, DatabaseError> {
    // SELECT {cols}
    // FROM notes
    // LEFT JOIN note_scripts ON notes.script_root = note_scripts.script_root
    // ORDER BY block_num ASC
    let q = schema::notes::table.left_join(
        schema::note_scripts::table
            .on(schema::notes::script_root.eq(schema::note_scripts::script_root.nullable())),
    );
    let raw: Vec<_> = SelectDsl::select(
        q,
        (NoteRecordRawRow::as_select(), schema::note_scripts::script.nullable()),
    )
    .order(schema::notes::committed_at.asc())
    .load::<(NoteRecordRawRow, Option<Vec<u8>>)>(conn)?;
    let records = vec_raw_try_into::<NoteRecord, NoteRecordWithScriptRawJoined>(
        raw.into_iter().map(NoteRecordWithScriptRawJoined::from),
    )?;
    Ok(records)
}

/// Select note inclusion proofs matching the `NoteId`, using the given [`SqliteConnection`].
///
/// # Returns
///
/// - Empty map if no matching `note`.
/// - Otherwise, note inclusion proofs, which `note_id` matches the `NoteId` as bytes.
pub(crate) fn select_note_inclusion_proofs(
    conn: &mut SqliteConnection,
    note_ids: &BTreeSet<NoteId>,
) -> Result<BTreeMap<NoteId, NoteInclusionProof>, DatabaseError> {
    QueryParamNoteIdLimit::check(note_ids.len())?;

    // SELECT
    //     block_num,
    //     note_id,
    //     batch_index,
    //     note_index,
    //     inclusion_path
    // FROM
    //     notes
    // WHERE
    //     note_id IN rarray(?1)
    // ORDER BY
    //     block_num ASC
    let noted_ids_serialized = serialize_vec(note_ids.iter());

    let raw_notes = SelectDsl::select(
        schema::notes::table,
        (
            schema::notes::committed_at,
            schema::notes::note_id,
            schema::notes::batch_index,
            schema::notes::note_index,
            schema::notes::inclusion_path,
        ),
    )
    .filter(schema::notes::note_id.eq_any(noted_ids_serialized))
    .order_by(schema::notes::committed_at.asc())
    .load::<(i64, Vec<u8>, i32, i32, Vec<u8>)>(conn)?;

    Result::<BTreeMap<_, _>, _>::from_iter(raw_notes.iter().map(
        |(block_num, note_id, batch_index, note_index, merkle_path)| {
            let note_id = NoteId::read_from_bytes(&note_id[..])?;
            let block_num = BlockNumber::from_raw_sql(*block_num)?;
            let node_index_in_block =
                BlockNoteIndex::new(raw_sql_to_idx(*batch_index), raw_sql_to_idx(*note_index))
                    .expect("batch and note index from DB should be valid")
                    .leaf_index_value();
            let merkle_path = SparseMerklePath::read_from_bytes(&merkle_path[..])?;
            let proof = NoteInclusionProof::new(block_num, node_index_in_block, merkle_path)?;
            Ok((note_id, proof))
        },
    ))
}

/// Returns a paginated batch of network notes that have not yet been consumed.
///
/// # Returns
///
/// A set of unconsumed network notes with maximum length of `size` and the page to get
/// the next set.
//
// Attention: uses the _implicit_ column `rowid`, which requires to use a few raw SQL nugget
// statements
#[allow(
    clippy::cast_sign_loss,
    reason = "We need custom SQL statements which has given types that we need to convert"
)]
pub(crate) fn unconsumed_network_notes(
    conn: &mut SqliteConnection,
    mut page: Page,
) -> Result<(Vec<NoteRecord>, Page), DatabaseError> {
    assert_eq!(
        NoteExecutionMode::Network as u8,
        0,
        "Hardcoded execution value must match query"
    );

    let rowid_sel = diesel::dsl::sql::<diesel::sql_types::BigInt>("notes.rowid");
    let rowid_sel_ge =
        diesel::dsl::sql::<diesel::sql_types::Bool>("notes.rowid >= ")
            .bind::<diesel::sql_types::BigInt, i64>(page.token.unwrap_or_default() as i64);

    // SELECT {}, rowid
    // FROM notes
    // LEFT JOIN note_scripts ON notes.script_root = note_scripts.script_root
    // WHERE
    //     execution_mode = 0 AND consumed_block_num = NULL AND rowid >= ?
    // ORDER BY rowid
    // LIMIT ?
    #[allow(
        clippy::items_after_statements,
        reason = "It's only relevant for a single call function"
    )]
    type RawLoadedTuple = (
        NoteRecordRawRow,
        Option<Vec<u8>>, // script
        i64,             // rowid (from sql::<BigInt>("notes.rowid"))
    );

    #[allow(
        clippy::items_after_statements,
        reason = "It's only relevant for a single call function"
    )]
    fn split_into_raw_note_record_and_implicit_row_id(
        tuple: RawLoadedTuple,
    ) -> (NoteRecordWithScriptRawJoined, i64) {
        let (note, script, row) = tuple;
        let combined = NoteRecordWithScriptRawJoined::from((note, script));
        (combined, row)
    }

    let raw = SelectDsl::select(
        schema::notes::table.left_join(
            schema::note_scripts::table
                .on(schema::notes::script_root.eq(schema::note_scripts::script_root.nullable())),
        ),
        (
            NoteRecordRawRow::as_select(),
            schema::note_scripts::script.nullable(),
            rowid_sel.clone(),
        ),
    )
    .filter(schema::notes::execution_mode.eq(NoteExecutionMode::Network.to_raw_sql()))
    .filter(schema::notes::consumed_at.is_null())
    .filter(rowid_sel_ge)
    .order(rowid_sel.asc())
    .limit(page.size.get() as i64 + 1)
    .load::<RawLoadedTuple>(conn)?;

    let mut notes = Vec::with_capacity(page.size.into());
    for raw_item in raw {
        let (raw_item, row_id) = split_into_raw_note_record_and_implicit_row_id(raw_item);
        page.token = None;
        if notes.len() == page.size.get() {
            page.token = Some(row_id as u64);
            break;
        }
        notes.push(TryInto::<NoteRecord>::try_into(raw_item)?);
    }

    Ok((notes, page))
}

/// Returns a paginated batch of network notes for an account that are unconsumed by a specified
/// block number.
///
///  Notes that are created or consumed after the specified block are excluded from the result.
///
/// # Returns
///
/// A set of unconsumed network notes with maximum length of `size` and the page to get
/// the next set.
//
// Attention: uses the _implicit_ column `rowid`, which requires to use a few raw SQL nugget
// statements
#[allow(
    clippy::cast_sign_loss,
    reason = "We need custom SQL statements which has given types that we need to convert"
)]
#[allow(
    clippy::too_many_lines,
    reason = "Lines will be reduced when schema is updated to simplify logic"
)]
pub(crate) fn select_unconsumed_network_notes_by_tag(
    conn: &mut SqliteConnection,
    tag: u32,
    block_num: BlockNumber,
    mut page: Page,
) -> Result<(Vec<NoteRecord>, Page), DatabaseError> {
    assert_eq!(
        NoteExecutionMode::Network as u8,
        0,
        "Hardcoded execution value must match query"
    );

    let rowid_sel = diesel::dsl::sql::<diesel::sql_types::BigInt>("notes.rowid");
    let rowid_sel_ge =
        diesel::dsl::sql::<diesel::sql_types::Bool>("notes.rowid >= ")
            .bind::<diesel::sql_types::BigInt, i64>(page.token.unwrap_or_default() as i64);

    // SELECT {}, rowid
    // FROM notes
    // LEFT JOIN note_scripts ON notes.script_root = note_scripts.script_root
    // WHERE
    //  execution_mode = 0 AND tag = ?1 AND
    //  block_num <= ?2 AND
    //  (consumed_block_num IS NULL OR consumed_block_num > ?2) AND rowid >= ?3
    // ORDER BY rowid
    // LIMIT ?
    #[allow(
        clippy::items_after_statements,
        reason = "It's only relevant for a single call function"
    )]
    type RawLoadedTuple = (
        NoteRecordRawRow,
        Option<Vec<u8>>, // script
        i64,             // rowid (from sql::<BigInt>("notes.rowid"))
    );

    #[allow(
        clippy::items_after_statements,
        reason = "It's only relevant for a single call function"
    )]
    fn split_into_raw_note_record_and_implicit_row_id(
        tuple: RawLoadedTuple,
    ) -> (NoteRecordWithScriptRawJoined, i64) {
        let (note, script, row) = tuple;
        let combined = NoteRecordWithScriptRawJoined::from((note, script));
        (combined, row)
    }

    let raw = SelectDsl::select(
        schema::notes::table.left_join(
            schema::note_scripts::table
                .on(schema::notes::script_root.eq(schema::note_scripts::script_root.nullable())),
        ),
        (
            NoteRecordRawRow::as_select(),
            schema::note_scripts::script.nullable(),
            rowid_sel.clone(),
        ),
    )
    .filter(schema::notes::execution_mode.eq(NoteExecutionMode::Network.to_raw_sql()))
    .filter(schema::notes::tag.eq(tag as i32))
    .filter(schema::notes::committed_at.le(block_num.to_raw_sql()))
    .filter(
        schema::notes::consumed_at
            .is_null()
            .or(schema::notes::consumed_at.gt(block_num.to_raw_sql())),
    )
    .filter(rowid_sel_ge)
    .order(rowid_sel.asc())
    .limit(page.size.get() as i64 + 1)
    .load::<RawLoadedTuple>(conn)?;

    let mut notes = Vec::with_capacity(page.size.into());
    for raw_item in raw {
        let (raw_item, row_id) = split_into_raw_note_record_and_implicit_row_id(raw_item);
        page.token = None;
        if notes.len() == page.size.get() {
            page.token = Some(row_id as u64);
            break;
        }
        notes.push(TryInto::<NoteRecord>::try_into(raw_item)?);
    }

    Ok((notes, page))
}

/// Loads the data necessary for a note sync.
pub(crate) fn get_note_sync(
    conn: &mut SqliteConnection,
    block_num: BlockNumber,
    note_tags: &[u32],
) -> Result<NoteSyncUpdate, NoteSyncError> {
    QueryParamNoteTagLimit::check(note_tags.len()).map_err(DatabaseError::from)?;

    let notes = select_notes_since_block_by_tag_and_sender(conn, block_num, &[], note_tags)?;

    let block_header =
        select_block_header_by_block_num(conn, notes.first().map(|note| note.block_num))?
            .ok_or(NoteSyncError::EmptyBlockHeadersTable)?;
    Ok(NoteSyncUpdate { notes, block_header })
}

#[derive(Debug, Clone, PartialEq, Selectable, Queryable, QueryableByName)]
#[diesel(table_name = schema::notes)]
#[diesel(check_for_backend(Sqlite))]
pub struct NoteSyncRecordRawRow {
    pub committed_at: i64, // BlockNumber
    #[diesel(embed)]
    pub block_note_index: BlockNoteIndexRawRow,
    pub note_id: Vec<u8>, // BlobDigest
    #[diesel(embed)]
    pub metadata: NoteMetadataRawRow,
    pub inclusion_path: Vec<u8>, // SparseMerklePath
}

#[allow(clippy::cast_sign_loss, reason = "Indices are cast to usize for ease of use")]
impl TryInto<NoteSyncRecord> for NoteSyncRecordRawRow {
    type Error = DatabaseError;
    fn try_into(self) -> Result<NoteSyncRecord, Self::Error> {
        let block_num = BlockNumber::from_raw_sql(self.committed_at)?;
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

#[derive(Debug, Clone, PartialEq, Selectable, Queryable, QueryableByName)]
#[diesel(table_name = schema::notes)]
#[diesel(check_for_backend(Sqlite))]
pub struct NoteDetailsRawRow {
    pub assets: Option<Vec<u8>>,
    pub inputs: Option<Vec<u8>>,
    pub serial_num: Option<Vec<u8>>,
}

// Note: One cannot use `#[diesel(embed)]` to structure
// this, it will yield a significant amount of errors
// when used with join and debugging is painful to put it
// mildly.
#[derive(Debug, Clone, PartialEq, Queryable)]
pub struct NoteRecordWithScriptRawJoined {
    pub committed_at: i64,

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

impl From<(NoteRecordRawRow, Option<Vec<u8>>)> for NoteRecordWithScriptRawJoined {
    fn from((note, script): (NoteRecordRawRow, Option<Vec<u8>>)) -> Self {
        let NoteRecordRawRow {
            committed_at,
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
            committed_at,
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

impl TryInto<NoteRecord> for NoteRecordWithScriptRawJoined {
    type Error = DatabaseError;
    fn try_into(self) -> Result<NoteRecord, Self::Error> {
        // let (raw, script) = self;
        let raw = self;
        let NoteRecordWithScriptRawJoined {
            committed_at,

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
        let index = BlockNoteIndexRawRow { batch_index, note_index };
        let metadata = NoteMetadataRawRow {
            note_type,
            sender,
            tag,
            aux,
            execution_hint,
        };
        let details = NoteDetailsRawRow { assets, inputs, serial_num };

        let metadata = metadata.try_into()?;
        let committed_at = BlockNumber::from_raw_sql(committed_at)?;
        let note_id = Word::read_from_bytes(&note_id[..])?;
        let script = script.map(|script| NoteScript::read_from_bytes(&script[..])).transpose()?;
        let details = if let NoteDetailsRawRow {
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
            block_num: committed_at,
            note_index,
            note_id,
            metadata,
            details,
            inclusion_path,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Selectable, Queryable, QueryableByName)]
#[diesel(table_name = schema::notes)]
#[diesel(check_for_backend(Sqlite))]
pub struct NoteRecordRawRow {
    pub committed_at: i64,

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

#[derive(Debug, Clone, PartialEq, Selectable, Queryable, QueryableByName)]
#[diesel(table_name = schema::notes)]
#[diesel(check_for_backend(Sqlite))]
pub struct NoteMetadataRawRow {
    note_type: i32,
    sender: Vec<u8>, // AccountId
    tag: i32,
    aux: i64,
    execution_hint: i64,
}

#[allow(clippy::cast_sign_loss)]
impl TryInto<NoteMetadata> for NoteMetadataRawRow {
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
#[diesel(table_name = schema::notes)]
#[diesel(check_for_backend(Sqlite))]
pub struct BlockNoteIndexRawRow {
    pub batch_index: i32,
    pub note_index: i32, // index within batch
}

#[allow(clippy::cast_sign_loss, reason = "Indices are cast to usize for ease of use")]
impl TryInto<BlockNoteIndex> for BlockNoteIndexRawRow {
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
