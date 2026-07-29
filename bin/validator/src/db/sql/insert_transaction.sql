INSERT INTO validated_transactions (
    id,
    submission_scheme,
    submission_key_id,
    sealed_transaction_inputs
)
VALUES (?1, ?2, ?3, ?4)
ON CONFLICT DO NOTHING;
