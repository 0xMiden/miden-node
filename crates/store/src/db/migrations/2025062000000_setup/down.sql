-- Drop all tables in reverse order of creation (respecting foreign key dependencies)
DROP TABLE IF EXISTS transactions;
DROP TABLE IF EXISTS nullifiers;
DROP TABLE IF EXISTS account_vault_headers;
DROP TABLE IF EXISTS account_vault_assets;
DROP TABLE IF EXISTS account_storage_map_values;
DROP TABLE IF EXISTS note_scripts;
DROP TABLE IF EXISTS notes;
DROP TABLE IF EXISTS account_storage_headers;
DROP TABLE IF EXISTS accounts;
DROP TABLE IF EXISTS account_codes;
DROP TABLE IF EXISTS block_headers;
