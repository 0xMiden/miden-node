-- Returns the latest transaction recorded against an account.
SELECT last_tx_id FROM accounts WHERE account_id = ?1
