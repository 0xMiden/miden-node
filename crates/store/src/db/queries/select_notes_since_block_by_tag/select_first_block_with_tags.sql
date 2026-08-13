-- Returns the earliest block within the range that contains a note matching one of the tags.
--
-- Tags are bound as a single array parameter so the statement text stays constant regardless of how
-- many are requested; see `miden_node_db::sqlite::InList`.
SELECT committed_at
FROM notes
WHERE tag IN (SELECT value FROM rarray(?1))
  AND committed_at >= ?2
  AND committed_at <= ?3
ORDER BY committed_at ASC
LIMIT 1;
