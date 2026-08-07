-- Replace the `is_latest` flag on versioned account state with an explicit validity interval.
-- Each row is applicable for blocks in `[block_num, valid_until)`. Open-ended (current) rows use
-- the sentinel 9223372036854775807 (i64::MAX) rather than NULL so that every validity predicate
-- is a single range comparison that partial indexes can serve. On update, the write path closes
-- the previous row's interval by setting `valid_until` to the new row's `block_num`.

-- accounts ---------------------------------------------------------------------------------------

ALTER TABLE accounts ADD COLUMN valid_until BIGINT NOT NULL DEFAULT 9223372036854775807;

UPDATE accounts SET valid_until = next_versions.next_block_num
FROM (
    SELECT account_id, block_num,
           LEAD(block_num) OVER (PARTITION BY account_id ORDER BY block_num) AS next_block_num
    FROM accounts
) AS next_versions
WHERE accounts.account_id = next_versions.account_id
  AND accounts.block_num = next_versions.block_num
  AND next_versions.next_block_num IS NOT NULL;

DROP INDEX idx_accounts_latest_by_account;
DROP INDEX idx_accounts_latest_public_by_account;
DROP INDEX idx_accounts_latest_network_by_account;
DROP INDEX idx_accounts_latest_code_commitment;
-- Superseded by idx_accounts_code_validity below.
DROP INDEX idx_accounts_prune_code;

ALTER TABLE accounts DROP COLUMN is_latest;

CREATE INDEX idx_accounts_latest_by_account
    ON accounts(account_id)
    WHERE valid_until = 9223372036854775807;
CREATE INDEX idx_accounts_latest_public_by_account
    ON accounts(account_id)
    WHERE valid_until = 9223372036854775807 AND code_commitment IS NOT NULL;
CREATE INDEX idx_accounts_latest_network_by_account
    ON accounts(account_id)
    WHERE valid_until = 9223372036854775807 AND network_account_type = 1;
-- Covering index for prune_account_codes: codes referenced by rows still valid at or after the
-- pruning cutoff (`valid_until > cutoff`), which includes all open-ended rows.
CREATE INDEX idx_accounts_code_validity
    ON accounts(valid_until, code_commitment)
    WHERE code_commitment IS NOT NULL;

-- account_vault_assets ---------------------------------------------------------------------------

ALTER TABLE account_vault_assets ADD COLUMN valid_until BIGINT NOT NULL DEFAULT 9223372036854775807;

UPDATE account_vault_assets SET valid_until = next_versions.next_block_num
FROM (
    SELECT account_id, vault_key, block_num,
           LEAD(block_num) OVER (PARTITION BY account_id, vault_key ORDER BY block_num)
               AS next_block_num
    FROM account_vault_assets
) AS next_versions
WHERE account_vault_assets.account_id = next_versions.account_id
  AND account_vault_assets.vault_key = next_versions.vault_key
  AND account_vault_assets.block_num = next_versions.block_num
  AND next_versions.next_block_num IS NOT NULL;

DROP INDEX idx_account_vault_assets_latest_by_account_key;
DROP INDEX idx_vault_cleanup;

ALTER TABLE account_vault_assets DROP COLUMN is_latest;

CREATE INDEX idx_account_vault_assets_latest_by_account_key
    ON account_vault_assets(account_id, vault_key)
    WHERE valid_until = 9223372036854775807;
-- Drives history pruning: superseded rows ordered by the block at which they expired.
CREATE INDEX idx_vault_cleanup
    ON account_vault_assets(valid_until)
    WHERE valid_until != 9223372036854775807;

-- account_storage_map_values ---------------------------------------------------------------------

ALTER TABLE account_storage_map_values ADD COLUMN valid_until BIGINT NOT NULL DEFAULT 9223372036854775807;

UPDATE account_storage_map_values SET valid_until = next_versions.next_block_num
FROM (
    SELECT account_id, slot_name, key, block_num,
           LEAD(block_num) OVER (PARTITION BY account_id, slot_name, key ORDER BY block_num)
               AS next_block_num
    FROM account_storage_map_values
) AS next_versions
WHERE account_storage_map_values.account_id = next_versions.account_id
  AND account_storage_map_values.slot_name = next_versions.slot_name
  AND account_storage_map_values.key = next_versions.key
  AND account_storage_map_values.block_num = next_versions.block_num
  AND next_versions.next_block_num IS NOT NULL;

DROP INDEX idx_account_storage_map_latest_by_account_slot_key;
DROP INDEX idx_storage_cleanup;

ALTER TABLE account_storage_map_values DROP COLUMN is_latest;

CREATE INDEX idx_account_storage_map_latest_by_account_slot_key
    ON account_storage_map_values(account_id, slot_name, key)
    WHERE valid_until = 9223372036854775807;
-- Drives history pruning: superseded rows ordered by the block at which they expired.
CREATE INDEX idx_storage_cleanup
    ON account_storage_map_values(valid_until)
    WHERE valid_until != 9223372036854775807;
