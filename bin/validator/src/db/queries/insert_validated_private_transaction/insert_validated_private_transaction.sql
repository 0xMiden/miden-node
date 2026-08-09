-- Inserts a validated transaction, ignoring the insert if the transaction is already recorded.
INSERT INTO validated_transactions (
    id,
    validator_id,
    chain_id,
    key_epoch,
    setup_context_id,
    format_version,
    cipher_nonce,
    encrypted_record,
    encrypted_record_key
)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
ON CONFLICT DO NOTHING;
