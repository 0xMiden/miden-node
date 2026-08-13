-- Returns the page of nullifiers following the cursor, ordered by nullifier for stable pagination.
--
-- The cursor is a bare `>` comparison rather than a nullable parameter so the range scan can use the
-- primary key index; pagination over the whole table would otherwise be quadratic.
SELECT nullifier, block_num
FROM nullifiers
WHERE nullifier > ?2
ORDER BY nullifier ASC
LIMIT ?1;
