use super::*;

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
    let raw: Vec<_> =
        SelectDsl::select(q, (NoteRecordRaw::as_select(), schema::note_scripts::script.nullable()))
            .load::<(NoteRecordRaw, Option<Vec<u8>>)>(conn)?;
    let records = vec_raw_try_into::<NoteRecord, NoteRecordWithScriptRaw>(
        raw.into_iter().map(NoteRecordWithScriptRaw::from),
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
    let raw: Vec<_> =
        SelectDsl::select(q, (NoteRecordRaw::as_select(), schema::note_scripts::script.nullable()))
            .order(schema::notes::committed_at.asc())
            .load::<(NoteRecordRaw, Option<Vec<u8>>)>(conn)?;
    let records = vec_raw_try_into::<NoteRecord, NoteRecordWithScriptRaw>(
        raw.into_iter().map(NoteRecordWithScriptRaw::from),
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
        NoteRecordRaw,
        Option<Vec<u8>>, // script
        i64,             // rowid (from sql::<BigInt>("notes.rowid"))
    );

    #[allow(
        clippy::items_after_statements,
        reason = "It's only relevant for a single call function"
    )]
    fn split_into_raw_note_record_and_implicit_row_id(
        tuple: RawLoadedTuple,
    ) -> (NoteRecordWithScriptRaw, i64) {
        let (note, script, row) = tuple;
        let combined = NoteRecordWithScriptRaw::from((note, script));
        (combined, row)
    }

    let raw = SelectDsl::select(
        schema::notes::table.left_join(
            schema::note_scripts::table
                .on(schema::notes::script_root.eq(schema::note_scripts::script_root.nullable())),
        ),
        (
            NoteRecordRaw::as_select(),
            schema::note_scripts::script.nullable(),
            rowid_sel.clone(),
        ),
    )
    .filter(schema::notes::execution_mode.eq(0_i32))
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
        NoteRecordRaw,
        Option<Vec<u8>>, // script
        i64,             // rowid (from sql::<BigInt>("notes.rowid"))
    );

    #[allow(
        clippy::items_after_statements,
        reason = "It's only relevant for a single call function"
    )]
    fn split_into_raw_note_record_and_implicit_row_id(
        tuple: RawLoadedTuple,
    ) -> (NoteRecordWithScriptRaw, i64) {
        let (note, script, row) = tuple;
        let combined = NoteRecordWithScriptRaw::from((note, script));
        (combined, row)
    }

    let raw = SelectDsl::select(
        schema::notes::table.left_join(
            schema::note_scripts::table
                .on(schema::notes::script_root.eq(schema::note_scripts::script_root.nullable())),
        ),
        (
            NoteRecordRaw::as_select(),
            schema::note_scripts::script.nullable(),
            rowid_sel.clone(),
        ),
    )
    .filter(schema::notes::execution_mode.eq(0_i32))
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
