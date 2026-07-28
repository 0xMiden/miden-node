-- Existing rows cannot supply the accepted client ciphertext. Refuse to discard them silently:
-- operators with validated rows must rebootstrap the validator database before deployment.
CREATE TABLE encrypted_submission_cutover (
    legacy_row_count  BIGINT NOT NULL CHECK (legacy_row_count = 0)
);
INSERT INTO encrypted_submission_cutover
SELECT COUNT(*) FROM validated_transactions;

DROP TABLE validated_transactions;
DROP TABLE encrypted_submission_cutover;

-- This Phase 1 record keeps the client envelope as a stand-in. Phase 2 will replace it with
-- validated transaction inputs encrypted under a fresh content key protected by Golden EHTDH1.
CREATE TABLE validated_transactions (
    id                         BLOB NOT NULL,
    submission_scheme          BIGINT NOT NULL,
    submission_key_id          BLOB NOT NULL,
    sealed_transaction_inputs  BLOB NOT NULL,
    PRIMARY KEY (id)
) WITHOUT ROWID;
