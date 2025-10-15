CREATE TABLE transactions (
    transaction_id               BLOB    NOT NULL,
    account_id                   BLOB    NOT NULL,
    block_num                    INTEGER NOT NULL, -- Block number in which the transaction was included.
    initial_state_commitment     BLOB    NOT NULL, -- State of the account before applying the transaction.
    final_state_commitment       BLOB    NOT NULL, -- State of the account after applying the transaction.
    input_notes                  BLOB    NOT NULL, -- Serialized vector with the Nullifier of the input notes.
    output_notes                 BLOB    NOT NULL, -- Serialized vector with the NoteId of the output notes.
    size_in_bytes                INTEGER NOT NULL, -- Estimated size of the row in bytes, considering the size of the input and output notes.

    PRIMARY KEY (transaction_id),
) WITHOUT ROWID;

CREATE INDEX idx_transactions_account_id ON transactions(account_id);
CREATE INDEX idx_transactions_block_num ON transactions(block_num);
