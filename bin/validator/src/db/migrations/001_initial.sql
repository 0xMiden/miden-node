CREATE TABLE validated_transactions (
    -- Transaction ID, unique within this validator's database.
    id                    BLOB NOT NULL,
    -- Signing public key of the validator that produced this record.
    validator_id          BLOB NOT NULL,
    -- Genesis commitment of the network that produced the transaction.
    chain_id              BLOB NOT NULL,
    -- Operator-chosen epoch of the Golden storage key.
    key_epoch             BLOB NOT NULL,
    -- Identifier of the Golden DKG setup needed to combine shares.
    setup_context_id      BLOB NOT NULL,
    -- Version of the context and encryption format. Version 1 uses XChaCha20-Poly1305.
    format_version        BIGINT NOT NULL,
    -- XChaCha20-Poly1305 nonce.
    cipher_nonce          BLOB NOT NULL,
    -- Authenticated encryption of the validated transaction inputs.
    encrypted_record      BLOB NOT NULL,
    -- Golden EHTDH1 encryption of the key for encrypted_record.
    encrypted_record_key  BLOB NOT NULL,
    PRIMARY KEY (id),
    CHECK (length(id) = 32),
    CHECK (length(validator_id) = 33),
    CHECK (length(chain_id) = 32),
    CHECK (length(key_epoch) = 32),
    CHECK (length(setup_context_id) = 32),
    CHECK (format_version = 1),
    CHECK (length(cipher_nonce) = 24),
    CHECK (length(encrypted_record) >= 16),
    CHECK (length(encrypted_record_key) > 0)
) WITHOUT ROWID;

CREATE INDEX idx_validated_transactions_key_epoch
ON validated_transactions(key_epoch);

CREATE INDEX idx_validated_transactions_setup_context_id
ON validated_transactions(setup_context_id);

CREATE TABLE block_headers (
    block_num    BIGINT PRIMARY KEY,
    block_header BLOB NOT NULL
) WITHOUT ROWID;
