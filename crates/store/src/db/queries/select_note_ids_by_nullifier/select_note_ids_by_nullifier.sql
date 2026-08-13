-- Maps the given nullifiers to their note ids.
--
-- Only public notes store a nullifier, so private notes never match.
SELECT nullifier, note_id
FROM notes
WHERE nullifier IN (SELECT value FROM rarray(?1));
