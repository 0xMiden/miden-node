-- Returns the sync records for the given notes, oldest block first.
SELECT committed_at, batch_index, note_index, note_id, note_type, sender, tag, attachment,
       inclusion_path
FROM notes
WHERE note_id IN (SELECT value FROM rarray(?1))
ORDER BY committed_at ASC;
