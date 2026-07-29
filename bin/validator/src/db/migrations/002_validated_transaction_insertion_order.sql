ALTER TABLE validated_transactions
ADD COLUMN insertion_sequence INTEGER NOT NULL DEFAULT 0;

-- The original insertion order is unavailable for existing rows. Assign a stable order by
-- transaction ID so every migrated database has the same result.
UPDATE validated_transactions AS current
SET insertion_sequence = (
    SELECT COUNT(*)
    FROM validated_transactions AS preceding
    WHERE preceding.id <= current.id
);

CREATE UNIQUE INDEX idx_validated_transactions_insertion_sequence
ON validated_transactions(insertion_sequence);
