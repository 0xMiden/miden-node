-- Returns the page of latest account commitments following the cursor.
--
-- The cursor is a bare `>` comparison rather than a nullable parameter so the range scan can use the
-- index on `account_id`.
SELECT account_id, account_commitment
FROM accounts
WHERE valid_until = ?2
  AND account_id > ?3
ORDER BY account_id ASC
LIMIT ?1;
