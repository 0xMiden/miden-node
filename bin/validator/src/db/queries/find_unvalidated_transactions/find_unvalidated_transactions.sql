-- Returns the subset of the given transaction ids that have already been validated.
--
-- The ids are bound as a single array parameter and expanded with `rarray` so that the statement
-- text is independent of the number of ids.
SELECT id
FROM validated_transactions
WHERE id IN (SELECT value FROM rarray(?1));
