CREATE TABLE block_headers (
    block_num    INTEGER NOT NULL,
    block_header BLOB    NOT NULL,

    PRIMARY KEY (block_num),
    CONSTRAINT block_header_block_num_is_u32 CHECK (block_num BETWEEN 0 AND 0xFFFFFFFF)
);


CREATE TABLE accounts (
    account_id                              BLOB NOT NULL,
    network_account_id_prefix               INTEGER NULL, -- 30-bit account ID prefix, only filled for network accounts
    account_commitment                      BLOB NOT NULL,
    block_num                               INTEGER NOT NULL,
    details                                 BLOB,

    PRIMARY KEY (account_id),
    FOREIGN KEY (block_num) REFERENCES block_headers(block_num)
) WITHOUT ROWID;

CREATE INDEX idx_accounts_network_prefix ON accounts(network_account_id_prefix) WHERE network_account_id_prefix IS NOT NULL;

CREATE TABLE notes (
    block_num       INTEGER NOT NULL,
    batch_index     INTEGER NOT NULL, -- Index of batch in block, starting from 0
    note_index      INTEGER NOT NULL, -- Index of note in batch, starting from 0
    note_id         BLOB    NOT NULL,
    note_type       INTEGER NOT NULL, -- 1-Public (0b01), 2-Private (0b10), 3-Encrypted (0b11)
    sender          BLOB    NOT NULL,
    tag             INTEGER NOT NULL,
    execution_mode  INTEGER NOT NULL, -- 0-Network, 1-Local
    aux             INTEGER NOT NULL,
    execution_hint  INTEGER NOT NULL,
    inclusion_path  BLOB NOT NULL,    -- Serialized sparse Merkle path of the note in the block's note tree
    consumed        INTEGER NOT NULL, -- boolean
    nullifier       BLOB,             -- Only known for public notes, null for private notes
    assets          BLOB,
    inputs          BLOB,
    script_root     BLOB,
    serial_num      BLOB,

    PRIMARY KEY (block_num, batch_index, note_index),
    FOREIGN KEY (block_num) REFERENCES block_headers(block_num),
    FOREIGN KEY (sender) REFERENCES accounts(account_id),
    FOREIGN KEY (script_root) REFERENCES note_scripts(script_root),
    CONSTRAINT notes_type_in_enum CHECK (note_type BETWEEN 1 AND 3),
    CONSTRAINT notes_execution_mode_in_enum CHECK (execution_mode BETWEEN 0 AND 1),
    CONSTRAINT notes_consumed_is_bool CHECK (execution_mode BETWEEN 0 AND 1),
    CONSTRAINT notes_batch_index_is_u32 CHECK (batch_index BETWEEN 0 AND 0xFFFFFFFF),
    CONSTRAINT notes_note_index_is_u32 CHECK (note_index BETWEEN 0 AND 0xFFFFFFFF)
);

CREATE INDEX idx_notes_note_id ON notes(note_id);
CREATE INDEX idx_notes_sender ON notes(sender, block_num);
CREATE INDEX idx_notes_tag ON notes(tag, block_num);
CREATE INDEX idx_notes_nullifier ON notes(nullifier);
CREATE INDEX idx_unconsumed_network_notes ON notes(execution_mode, consumed);

CREATE TABLE note_scripts (
    script_root BLOB NOT NULL,
    script      BLOB NOT NULL,

    PRIMARY KEY (script_root)
) WITHOUT ROWID;

CREATE TABLE account_deltas (
    account_id  BLOB NOT NULL,
    block_num   INTEGER NOT NULL,
    nonce       INTEGER NOT NULL,

    PRIMARY KEY (account_id, block_num),
    FOREIGN KEY (account_id) REFERENCES accounts(account_id),
    FOREIGN KEY (block_num) REFERENCES block_headers(block_num)
) WITHOUT ROWID;

CREATE TABLE account_storage_slot_updates (
    account_id  BLOB NOT NULL,
    block_num   INTEGER NOT NULL,
    slot        INTEGER NOT NULL,
    value       BLOB    NOT NULL,

    PRIMARY KEY (account_id, block_num, slot),
    FOREIGN KEY (account_id, block_num) REFERENCES account_deltas (account_id, block_num)
) WITHOUT ROWID;

CREATE TABLE account_storage_map_updates (
    account_id  BLOB NOT NULL,
    block_num   INTEGER NOT NULL,
    slot        INTEGER NOT NULL,
    key         BLOB    NOT NULL,
    value       BLOB    NOT NULL,

    PRIMARY KEY (account_id, block_num, slot, key),
    FOREIGN KEY (account_id, block_num) REFERENCES account_deltas (account_id, block_num)
) WITHOUT ROWID;

CREATE TABLE account_fungible_asset_deltas (
    account_id  BLOB NOT NULL,
    block_num   INTEGER NOT NULL,
    faucet_id   BLOB NOT NULL,
    delta       INTEGER NOT NULL,

    PRIMARY KEY (account_id, block_num, faucet_id),
    FOREIGN KEY (account_id, block_num) REFERENCES account_deltas (account_id, block_num)
) WITHOUT ROWID;

CREATE TABLE account_non_fungible_asset_updates (
    account_id  BLOB NOT NULL,
    block_num   INTEGER NOT NULL,
    vault_key   BLOB NOT NULL,
    is_remove   INTEGER NOT NULL, -- 0 - add, 1 - remove

    PRIMARY KEY (account_id, block_num, vault_key),
    FOREIGN KEY (account_id, block_num) REFERENCES account_deltas (account_id, block_num)
) WITHOUT ROWID;

CREATE TABLE nullifiers (
    nullifier        BLOB    NOT NULL,
    nullifier_prefix INTEGER NOT NULL,
    block_num        INTEGER NOT NULL,

    PRIMARY KEY (nullifier),
    FOREIGN KEY (block_num) REFERENCES block_headers(block_num),
    CONSTRAINT nullifiers_nullifier_is_digest CHECK (length(nullifier) = 32),
    CONSTRAINT nullifiers_nullifier_prefix_is_u16 CHECK (nullifier_prefix BETWEEN 0 AND 0xFFFF)
) WITHOUT ROWID;

CREATE INDEX idx_nullifiers_prefix ON nullifiers(nullifier_prefix);
CREATE INDEX idx_nullifiers_block_num ON nullifiers(block_num);

CREATE TABLE transactions (
    transaction_id BLOB    NOT NULL,
    account_id     BLOB    NOT NULL,
    block_num      INTEGER NOT NULL,

    PRIMARY KEY (transaction_id),
    FOREIGN KEY (account_id) REFERENCES accounts(account_id),
    FOREIGN KEY (block_num) REFERENCES block_headers(block_num)
) WITHOUT ROWID;

CREATE INDEX idx_transactions_account_id ON transactions(account_id);
CREATE INDEX idx_transactions_block_num ON transactions(block_num);
