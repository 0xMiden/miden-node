SELECT
    chain_id,
    key_epoch,
    id,
    validator_id,
    setup_context_id,
    format_version,
    cipher_nonce,
    encrypted_record,
    encrypted_record_key
FROM validated_transactions
ORDER BY insertion_sequence;
