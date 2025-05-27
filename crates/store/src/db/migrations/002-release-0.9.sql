-- Create new table for note scripts
CREATE TABLE note_scripts (
    script_root BLOB NOT NULL,
    script      BLOB NOT NULL,

    PRIMARY KEY (script_root)
) STRICT, WITHOUT ROWID;

-- 30-bit account ID prefix, only filled for network accounts
ALTER TABLE accounts ADD COLUMN network_account_id_prefix INTEGER NULL;
CREATE INDEX IF NOT EXISTS idx_accounts_network_prefix ON accounts(network_account_id_prefix) WHERE network_account_id_prefix IS NOT NULL;

-- Create the new notes table with the updated schema
CREATE TABLE notes_new (
    block_num      INTEGER NOT NULL,
    batch_index    INTEGER NOT NULL, -- Index of batch in block, starting from 0
    note_index     INTEGER NOT NULL, -- Index of note in batch, starting from 0
    note_id        BLOB    NOT NULL,
    note_type      INTEGER NOT NULL, -- 1-Public (0b01), 2-Private (0b10), 3-Encrypted (0b11)
    sender         BLOB    NOT NULL,
    tag            INTEGER NOT NULL,
    execution_mode INTEGER NOT NULL, -- 0-Network, 1-Local
    aux            INTEGER NOT NULL,
    execution_hint INTEGER NOT NULL,
    merkle_path    BLOB    NOT NULL,
    consumed       INTEGER NOT NULL, -- boolean
    nullifier      BLOB,             -- Only known for public notes, null for private notes
    details        BLOB,             -- Temporary column for the migration
    assets         BLOB,
    inputs         BLOB,
    script_root    BLOB,
    serial_num     BLOB,

    PRIMARY KEY (block_num, batch_index, note_index),
    FOREIGN KEY (block_num) REFERENCES block_headers(block_num),
    FOREIGN KEY (sender) REFERENCES accounts(account_id),
    FOREIGN KEY (script_root) REFERENCES note_scripts(script_root),
    CONSTRAINT notes_type_in_enum CHECK (note_type BETWEEN 1 AND 3),
    CONSTRAINT notes_execution_mode_in_enum CHECK (execution_mode BETWEEN 0 AND 1),
    CONSTRAINT notes_consumed_is_bool CHECK (execution_mode BETWEEN 0 AND 1),
    CONSTRAINT notes_batch_index_is_u32 CHECK (batch_index BETWEEN 0 AND 0xFFFFFFFF),
    CONSTRAINT notes_note_index_is_u32 CHECK (note_index BETWEEN 0 AND 0xFFFFFFFF)
) STRICT;

-- Copy data from the old notes table to the new one
INSERT INTO notes_new (
    block_num, batch_index, note_index, note_id, note_type, sender, tag,
    execution_mode, aux, execution_hint, merkle_path, consumed, nullifier,
    details, assets, inputs, script_root, serial_num
)
SELECT 
    block_num, batch_index, note_index, note_id, note_type, sender, tag,
    execution_mode, aux, execution_hint, merkle_path, consumed, nullifier,
    details, NULL, NULL, NULL, NULL
FROM notes;

-- Recreate indexes
CREATE INDEX idx_new_notes_note_id ON notes_new(note_id);
CREATE INDEX idx_new_notes_sender ON notes_new(sender, block_num);
CREATE INDEX idx_new_notes_tag ON notes_new(tag, block_num);
CREATE INDEX idx_new_notes_nullifier ON notes_new(nullifier);
CREATE INDEX idx_new_unconsumed_network_notes ON notes_new(execution_mode, consumed);
