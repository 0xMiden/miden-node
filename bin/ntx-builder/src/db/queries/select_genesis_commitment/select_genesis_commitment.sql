-- Reads the genesis block commitment from the singleton chain state row.
SELECT genesis_commitment FROM chain_state WHERE id = 0
