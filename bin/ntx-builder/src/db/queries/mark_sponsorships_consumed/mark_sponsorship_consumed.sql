-- Marks a FEE_SPONSORSHIP note as consumed by setting `committed_at` to the block number whose
-- committed body contained its nullifier. This covers both consumption alongside the feature note
-- and an external reclaim. Rows for nullifiers we never inserted are silently skipped (no match).
-- Rows are kept around (not deleted), mirroring the `notes` table lifecycle.
UPDATE sponsorship_notes
SET committed_at = ?2
WHERE nullifier = ?1 AND committed_at IS NULL
