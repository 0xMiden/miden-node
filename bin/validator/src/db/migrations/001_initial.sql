CREATE TABLE validated_transactions (
    id                         BLOB NOT NULL,
    submission_scheme          BIGINT NOT NULL,
    submission_key_id          BLOB NOT NULL,
    sealed_transaction_inputs  BLOB NOT NULL,
    PRIMARY KEY (id)
) WITHOUT ROWID;

CREATE TABLE block_headers (
    block_num    BIGINT PRIMARY KEY,
    block_header BLOB NOT NULL
) WITHOUT ROWID;
