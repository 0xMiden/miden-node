-- Inserts the singleton chain state row at bootstrap, seeding the tip columns from the genesis block
-- together with the genesis block commitment. The commitment satisfies the `NOT NULL` constraint at
-- insert time and is retained across all subsequent tip updates (see `update_chain_state_tip`).
INSERT INTO chain_state (
    id, block_num, block_header, chain_mmr, genesis_commitment, genesis_validator_keys
)
VALUES (0, ?1, ?2, ?3, ?4, ?5)
