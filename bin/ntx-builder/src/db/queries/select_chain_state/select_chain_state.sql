-- Reads the singleton chain state row, returning the persisted block number, header, and chain MMR.
SELECT block_num, block_header, chain_mmr FROM chain_state WHERE id = 0
