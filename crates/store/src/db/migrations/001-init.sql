-- Table for storing different settings in run-time, which need to persist over runs.
-- Note: we can store values of different types in the same `value` field.
CREATE TABLE
    settings
(
    name  TEXT NOT NULL,
    value ANY,

    PRIMARY KEY (name),
    CONSTRAINT settings_name_is_not_empty CHECK (length(name) > 0)
) STRICT, WITHOUT ROWID;

CREATE TABLE
    block_headers
(
    block_num    INTEGER NOT NULL,
    block_header BLOB    NOT NULL,

    PRIMARY KEY (block_num),
    CONSTRAINT block_header_block_num_is_u32 CHECK (block_num BETWEEN 0 AND 0xFFFFFFFF)
) STRICT;

CREATE TABLE
    notes
(
    block_num      INTEGER NOT NULL,
    batch_index    INTEGER NOT NULL, -- Index of batch in block, starting from 0
    note_index     INTEGER NOT NULL, -- Index of note in batch, starting from 0
    note_id        BLOB    NOT NULL,
    note_type      INTEGER NOT NULL, -- 1-Public (0b01), 2-Private (0b10), 3-Encrypted (0b11)
    sender         BLOB NOT NULL,
    tag            INTEGER NOT NULL,
    aux            INTEGER NOT NULL,
    execution_hint INTEGER NOT NULL,
    merkle_path    BLOB    NOT NULL,
    details        BLOB,

    PRIMARY KEY (block_num, batch_index, note_index),
    FOREIGN KEY (block_num) REFERENCES block_headers(block_num),
    CONSTRAINT notes_type_in_enum CHECK (note_type BETWEEN 1 AND 3),
    CONSTRAINT notes_block_num_is_u32 CHECK (block_num BETWEEN 0 AND 0xFFFFFFFF),
    CONSTRAINT notes_batch_index_is_u32 CHECK (batch_index BETWEEN 0 AND 0xFFFFFFFF),
    CONSTRAINT notes_note_index_is_u32 CHECK (note_index BETWEEN 0 AND 0xFFFFFFFF)
) STRICT, WITHOUT ROWID;

CREATE TABLE
    accounts
(
    account_id   BLOB NOT NULL,
    account_hash BLOB    NOT NULL,
    block_num    INTEGER NOT NULL,
    details      BLOB,

    PRIMARY KEY (account_id),
    FOREIGN KEY (block_num) REFERENCES block_headers(block_num),
    CONSTRAINT accounts_block_num_is_u32 CHECK (block_num BETWEEN 0 AND 0xFFFFFFFF)
) STRICT;

CREATE TABLE
    account_deltas
(
    account_id  BLOB NOT NULL,
    block_num   INTEGER NOT NULL,
    nonce       INTEGER NOT NULL,

    PRIMARY KEY (account_id, block_num),
    FOREIGN KEY (account_id) REFERENCES accounts(account_id),
    FOREIGN KEY (block_num) REFERENCES block_headers(block_num)
) STRICT, WITHOUT ROWID;

CREATE TABLE
    account_storage_slot_updates
(
    account_id  BLOB NOT NULL,
    block_num   INTEGER NOT NULL,
    slot        INTEGER NOT NULL,
    value       BLOB    NOT NULL,

    PRIMARY KEY (account_id, block_num, slot),
    FOREIGN KEY (account_id, block_num) REFERENCES account_deltas (account_id, block_num)
) STRICT, WITHOUT ROWID;

CREATE TABLE
    account_storage_map_updates
(
    account_id  BLOB NOT NULL,
    block_num   INTEGER NOT NULL,
    slot        INTEGER NOT NULL,
    key         BLOB    NOT NULL,
    value       BLOB    NOT NULL,

    PRIMARY KEY (account_id, block_num, slot, key),
    FOREIGN KEY (account_id, block_num) REFERENCES account_deltas (account_id, block_num)
) STRICT, WITHOUT ROWID;

CREATE TABLE
    account_fungible_asset_deltas
(
    account_id  BLOB NOT NULL,
    block_num   INTEGER NOT NULL,
    faucet_id   INTEGER NOT NULL,
    delta       INTEGER NOT NULL,

    PRIMARY KEY (account_id, block_num, faucet_id),
    FOREIGN KEY (account_id, block_num) REFERENCES account_deltas (account_id, block_num)
) STRICT, WITHOUT ROWID;

CREATE TABLE
    account_non_fungible_asset_updates
(
    account_id  BLOB NOT NULL,
    block_num   INTEGER NOT NULL,
    vault_key   BLOB NOT NULL,
    is_remove   INTEGER NOT NULL, -- 0 - add, 1 - remove

    PRIMARY KEY (account_id, block_num, vault_key),
    FOREIGN KEY (account_id, block_num) REFERENCES account_deltas (account_id, block_num)
) STRICT, WITHOUT ROWID;

CREATE TABLE
    nullifiers
(
    nullifier        BLOB    NOT NULL,
    nullifier_prefix INTEGER NOT NULL,
    block_num        INTEGER NOT NULL,

    PRIMARY KEY (nullifier),
    FOREIGN KEY (block_num) REFERENCES block_headers(block_num),
    CONSTRAINT nullifiers_nullifier_is_digest CHECK (length(nullifier) = 32),
    CONSTRAINT nullifiers_nullifier_prefix_is_u16 CHECK (nullifier_prefix BETWEEN 0 AND 0xFFFF),
    CONSTRAINT nullifiers_block_num_is_u32 CHECK (block_num BETWEEN 0 AND 0xFFFFFFFF)
) STRICT, WITHOUT ROWID;

CREATE TABLE
    transactions
(
    transaction_id BLOB    NOT NULL,
    account_id     INTEGER NOT NULL,
    block_num      INTEGER NOT NULL,

    PRIMARY KEY (transaction_id),
    FOREIGN KEY (block_num) REFERENCES block_headers(block_num),
    CONSTRAINT transactions_block_num_is_u32 CHECK (block_num BETWEEN 0 AND 0xFFFFFFFF)
) STRICT, WITHOUT ROWID;

CREATE INDEX idx_transactions_account_id ON transactions(account_id);
CREATE INDEX idx_transactions_block_num ON transactions(block_num);
