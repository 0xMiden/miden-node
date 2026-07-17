-- Returns whether the account has any pending note: unconsumed and within the per-note attempt
-- budget. Tests for existence in SQL and deserializes nothing.
SELECT EXISTS (
    SELECT 1 FROM notes
    WHERE account_id = ?1 AND committed_at IS NULL AND attempt_count < ?2
)
