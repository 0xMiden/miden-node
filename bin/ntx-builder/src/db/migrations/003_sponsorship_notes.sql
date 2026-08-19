-- FEE_SPONSORSHIP notes, indexed by the feature note they pay the fee for.
--
-- Sponsorship notes have no backoff lifecycle of their own: execution failures are attributed to
-- the feature note, whose row in `notes` carries the attempt tracking.
CREATE TABLE sponsorship_notes (
    -- Nullifier bytes (32 bytes). Primary key.
    nullifier       BLOB    PRIMARY KEY,
    -- Note ID bytes.
    note_id         BLOB    NOT NULL,
    -- Note ID of the feature note this sponsorship pays for. Joins against `notes.note_id`.
    feature_note_id BLOB    NOT NULL,
    -- Serialized Note.
    note_data       BLOB    NOT NULL,
    -- Block height at or after which the reclaimer may reclaim the note. NULL when reclaim is
    -- disabled.
    reclaim_height  BIGINT,
    -- Block number in which the note's nullifier was observed in a committed block. NULL while
    -- the note is still pending consumption.
    committed_at    BIGINT,

    CONSTRAINT sponsorship_notes_reclaim_height_is_u32
        CHECK (reclaim_height BETWEEN 0 AND 0xFFFFFFFF),
    CONSTRAINT sponsorship_notes_committed_at_is_u32
        CHECK (committed_at BETWEEN 0 AND 0xFFFFFFFF)
) WITHOUT ROWID;

-- Partial index covering the selection-time join (`feature_note_id = ? AND committed_at IS NULL`).
CREATE INDEX idx_sponsorship_notes_feature ON sponsorship_notes(feature_note_id)
    WHERE committed_at IS NULL;
