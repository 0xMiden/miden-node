-- Returns the inclusion proof data for the given notes, restricted to those already committed.
SELECT committed_at, note_id, batch_index, note_index, inclusion_path
FROM notes
WHERE note_id IN (SELECT value FROM rarray(?1))
  AND committed_at <= ?2
ORDER BY committed_at ASC;
