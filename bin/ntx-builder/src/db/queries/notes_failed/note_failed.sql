-- Marks a note as failed by incrementing `attempt_count`, setting `last_attempt`, and storing the
-- latest error message.
UPDATE notes
SET attempt_count = attempt_count + 1, last_attempt = ?2, last_error = ?3
WHERE nullifier = ?1
