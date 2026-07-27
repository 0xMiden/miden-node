UPDATE private_records
SET block_num = ?1
WHERE transaction_id = ?2;
