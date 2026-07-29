-- Preserve the validator signing keys from genesis as the trust root for transaction encryption
-- key attestations. Existing databases must be re-bootstrapped because their genesis header was
-- not retained after the chain tip advanced.
ALTER TABLE chain_state ADD COLUMN genesis_validator_keys BLOB;
