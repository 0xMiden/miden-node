CREATE TABLE protocol_configs (
    commitment BLOB NOT NULL PRIMARY KEY CHECK (length(commitment) = 32),
    protocol_config BLOB NOT NULL
) WITHOUT ROWID;
