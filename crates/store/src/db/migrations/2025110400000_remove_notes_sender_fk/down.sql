-- Restore foreign key constraint on notes.sender
-- This down migration recreates the original table structure with the foreign key

-- Create notes table with the sender foreign key constraint
CREATE TABLE notes_new (
    committed_at             INTEGER NOT NULL, -- Block number when the note was committed
    batch_index              INTEGER NOT NULL, -- Index of batch in block, starting from 0
    note_index               INTEGER NOT NULL, -- Index of note in batch, starting from 0
    note_id                  BLOB    NOT NULL,
    note_type                INTEGER NOT NULL, -- 1-Public (0b01), 2-Private (0b10), 3-Encrypted (0b11)
    sender                   BLOB    NOT NULL,
    tag                      INTEGER NOT NULL,
    execution_mode           INTEGER NOT NULL, -- 0-Network, 1-Local
    aux                      INTEGER NOT NULL,
    execution_hint           INTEGER NOT NULL,
    inclusion_path           BLOB NOT NULL,    -- Serialized sparse Merkle path of the note in the block's note tree
    consumed_at              INTEGER,          -- Block number when the note was consumed
    nullifier                BLOB,             -- Only known for public notes, null for private notes
    assets                   BLOB,
    inputs                   BLOB,
    script_root              BLOB,
    serial_num               BLOB,

    PRIMARY KEY (committed_at, batch_index, note_index),
    FOREIGN KEY (committed_at) REFERENCES block_headers(block_num),
    FOREIGN KEY (sender) REFERENCES accounts(account_id),
    FOREIGN KEY (script_root) REFERENCES note_scripts(script_root),
    CONSTRAINT notes_type_in_enum CHECK (note_type BETWEEN 1 AND 3),
    CONSTRAINT notes_execution_mode_in_enum CHECK (execution_mode BETWEEN 0 AND 1),
    CONSTRAINT notes_consumed_at_is_u32 CHECK (consumed_at BETWEEN 0 AND 0xFFFFFFFF),
    CONSTRAINT notes_batch_index_is_u32 CHECK (batch_index BETWEEN 0 AND 0xFFFFFFFF),
    CONSTRAINT notes_note_index_is_u32 CHECK (note_index BETWEEN 0 AND 0xFFFFFFFF)
);

-- Copy data from old table to new table
INSERT INTO notes_new SELECT * FROM notes;

-- Drop old table
DROP TABLE notes;

-- Rename new table to notes
ALTER TABLE notes_new RENAME TO notes;

-- Recreate indexes
CREATE INDEX idx_notes_note_id ON notes(note_id);
CREATE INDEX idx_notes_sender ON notes(sender, committed_at);
CREATE INDEX idx_notes_tag ON notes(tag, committed_at);
CREATE INDEX idx_notes_nullifier ON notes(nullifier);
CREATE INDEX idx_unconsumed_network_notes ON notes(execution_mode, consumed_at);
