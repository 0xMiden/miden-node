-- Reads the validator signing keys retained from the genesis header.
SELECT genesis_validator_keys FROM chain_state WHERE id = 0
