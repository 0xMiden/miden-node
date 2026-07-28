INSERT INTO validated_transactions (
    id
)
VALUES (?1)
ON CONFLICT DO NOTHING;
