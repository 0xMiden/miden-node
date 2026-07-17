-- Returns the status for a note identified by its note ID.
SELECT last_error, attempt_count, last_attempt, committed_at FROM notes WHERE note_id = ?1
