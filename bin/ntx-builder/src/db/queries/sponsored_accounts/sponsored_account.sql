-- Resolves the account targeted by the (still unconsumed) feature note a FEE_SPONSORSHIP note is
-- bound to. No row matches when the feature note is unknown or already consumed.
SELECT account_id FROM notes
WHERE note_id = ?1 AND committed_at IS NULL
