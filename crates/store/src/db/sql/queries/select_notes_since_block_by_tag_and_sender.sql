-- Selects new notes matching the tags and account IDs search criteria.
SELECT
    block_num,
    batch_index,
    note_index,
    note_id,
    note_type,
    sender,
    tag,
    aux,
    execution_hint,
    inclusion_path
FROM
    notes
WHERE
    -- find the next block which contains at least one note with a matching tag or sender
    block_num = (
        SELECT
            block_num
        FROM
            notes
        WHERE
            (tag IN rarray(?1) OR sender IN rarray(?2)) AND
            block_num > ?3
        ORDER BY
            block_num ASC
    LIMIT 1) AND
    -- filter the block's notes and return only the ones matching the requested tags or senders
    (tag IN rarray(?1) OR sender IN rarray(?2))
