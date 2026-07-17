-- Returns the committed account state for the given network account.
SELECT account_data FROM accounts WHERE account_id = ?1
