-- Marks a note as consumed by setting `committed_at` to the block number whose committed body
-- contained its nullifier. Rows for nullifiers we never inserted are silently skipped (no match).
-- Rows are kept around (not deleted) so the `GetNetworkNoteStatus` endpoint can report the full
-- lifecycle of any note the ntx-builder has ever seen.
UPDATE notes
SET committed_at = ?2
WHERE nullifier = ?1 AND committed_at IS NULL
