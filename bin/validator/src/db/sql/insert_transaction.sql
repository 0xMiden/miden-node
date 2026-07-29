INSERT INTO validated_transactions (
    id,
    chain_id,
    key_epoch,
    setup_context_id,
    format_version,
    cipher_nonce,
    encrypted_record,
    encrypted_record_key
)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
ON CONFLICT DO NOTHING;
