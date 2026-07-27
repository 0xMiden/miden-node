INSERT INTO private_records (
    record_id,
    transaction_id,
    chain_id,
    key_epoch,
    setup_context_id,
    schema_version,
    block_num,
    cipher_id,
    cipher_nonce,
    encrypted_record,
    wrapped_content_key
)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
ON CONFLICT DO NOTHING;
