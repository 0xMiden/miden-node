CREATE TABLE private_records (
    record_id            BLOB NOT NULL,
    transaction_id       BLOB NOT NULL,
    chain_id             BLOB NOT NULL,
    key_epoch            BLOB NOT NULL,
    setup_context_id     BLOB NOT NULL,
    schema_version       BIGINT NOT NULL,
    block_num            BIGINT,
    cipher_id            BIGINT NOT NULL,
    cipher_nonce         BLOB NOT NULL,
    encrypted_record     BLOB NOT NULL,
    wrapped_content_key  BLOB NOT NULL,
    PRIMARY KEY (record_id),
    CHECK (length(record_id) = 32),
    CHECK (length(transaction_id) = 32),
    CHECK (length(chain_id) = 32),
    CHECK (length(key_epoch) = 32),
    CHECK (length(setup_context_id) = 32),
    CHECK (schema_version = 1),
    CHECK (block_num IS NULL OR block_num BETWEEN 0 AND 4294967295),
    CHECK (cipher_id = 1),
    CHECK (length(cipher_nonce) = 24),
    CHECK (length(encrypted_record) >= 16),
    CHECK (length(wrapped_content_key) > 0)
) WITHOUT ROWID;

CREATE INDEX idx_private_records_key_epoch
ON private_records(key_epoch);

CREATE INDEX idx_private_records_setup_context_id
ON private_records(setup_context_id);

CREATE INDEX idx_private_records_transaction_id
ON private_records(transaction_id);

CREATE INDEX idx_private_records_block_num
ON private_records(block_num)
WHERE block_num IS NOT NULL;
