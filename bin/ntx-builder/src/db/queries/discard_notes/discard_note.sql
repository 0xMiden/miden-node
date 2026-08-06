-- Marks a note as permanently unconsumable by pinning `attempt_count` to `max_attempts`, recording
-- the block at which it was discarded in `last_attempt`, and storing the reason in `last_error`.
UPDATE notes
SET attempt_count = ?2, last_attempt = ?3, last_error = ?4
WHERE nullifier = ?1
