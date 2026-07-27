SELECT
    chain_id,
    key_epoch,
    record_id,
    transaction_id,
    setup_context_id,
    schema_version,
    block_num,
    cipher_id,
    cipher_nonce,
    encrypted_record,
    wrapped_content_key
FROM private_records
WHERE block_num = ?1
ORDER BY record_id;
