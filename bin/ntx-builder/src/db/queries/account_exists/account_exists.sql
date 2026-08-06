-- Returns whether a committed state for the given account is tracked locally.
SELECT EXISTS (SELECT 1 FROM accounts WHERE account_id = ?1)
