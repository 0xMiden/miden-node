SELECT EXISTS(
    SELECT 1
    FROM validated_transactions
    WHERE id = ?1
);
