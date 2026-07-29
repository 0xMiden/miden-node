INSERT INTO validated_transactions (
    id,
    validator_id,
    chain_id,
    key_epoch,
    setup_context_id,
    format_version,
    cipher_nonce,
    encrypted_record,
    encrypted_record_key,
    insertion_sequence
)
VALUES (
    ?1,
    ?2,
    ?3,
    ?4,
    ?5,
    ?6,
    ?7,
    ?8,
    ?9,
    (SELECT COALESCE(MAX(insertion_sequence), 0) + 1 FROM validated_transactions)
)
ON CONFLICT DO NOTHING;
