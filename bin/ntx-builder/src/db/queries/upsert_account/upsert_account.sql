-- Inserts the committed account state, or updates an existing account's state. In both cases
-- `last_tx_id` is set to the transaction that produced this update.
INSERT INTO accounts (account_id, account_data, last_tx_id)
VALUES (?1, ?2, ?3)
ON CONFLICT(account_id) DO UPDATE SET
    account_data = excluded.account_data,
    last_tx_id = excluded.last_tx_id
