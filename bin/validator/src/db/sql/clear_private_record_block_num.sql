UPDATE private_records
SET block_num = NULL
WHERE block_num = ?1;
