SELECT
    submission_scheme,
    submission_key_id,
    sealed_transaction_inputs
FROM validated_transactions
WHERE id = ?1;
