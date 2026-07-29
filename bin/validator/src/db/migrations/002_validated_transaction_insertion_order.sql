ALTER TABLE validated_transactions
ADD COLUMN insertion_sequence INTEGER NOT NULL DEFAULT 0;

-- The original insertion order is unavailable for existing rows. Assign a stable order by
-- transaction ID so every migrated database has the same result.
CREATE TEMP TABLE validated_transaction_sequences (
    id BLOB PRIMARY KEY,
    insertion_sequence INTEGER NOT NULL
) WITHOUT ROWID;

INSERT INTO validated_transaction_sequences (id, insertion_sequence)
SELECT id, ROW_NUMBER() OVER (ORDER BY id)
FROM validated_transactions;

UPDATE validated_transactions
SET insertion_sequence = (
    SELECT insertion_sequence
    FROM validated_transaction_sequences
    WHERE validated_transaction_sequences.id = validated_transactions.id
);

DROP TABLE validated_transaction_sequences;

CREATE UNIQUE INDEX idx_validated_transactions_insertion_sequence
ON validated_transactions(insertion_sequence);
