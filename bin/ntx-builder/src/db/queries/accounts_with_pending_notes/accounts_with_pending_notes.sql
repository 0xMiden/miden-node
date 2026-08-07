-- Returns the distinct set of network accounts that currently have at least one pending note
-- (unconsumed and within the per-note attempt budget).
SELECT DISTINCT account_id FROM notes
WHERE committed_at IS NULL AND attempt_count < ?1
