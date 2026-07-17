-- Updates the tip columns of the singleton chain state row. The row is created once at bootstrap by
-- `insert_genesis_chain_state`, so this is a plain update; `genesis_commitment` is never touched.
UPDATE chain_state
SET block_num = ?1, block_header = ?2, chain_mmr = ?3
WHERE id = 0
