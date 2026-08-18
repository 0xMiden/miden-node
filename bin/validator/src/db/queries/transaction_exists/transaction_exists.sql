-- Returns whether a transaction with the given id has already been validated.
SELECT EXISTS(
    SELECT 1
    FROM validated_transactions
    WHERE id = ?1
);
