-- Returns the subset of the given note commitments that is already stored at or before the block.
SELECT note_id
FROM notes
WHERE note_id IN (SELECT value FROM rarray(?1))
  AND committed_at <= ?2;
