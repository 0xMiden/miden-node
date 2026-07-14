INSERT INTO validated_transactions (
    id,
    block_num,
    account_id,
    account_patch,
    input_notes,
    output_notes,
    initial_account_hash,
    final_account_hash
)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
ON CONFLICT DO NOTHING;
