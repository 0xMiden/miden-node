-- Inserts a note script (idempotent via INSERT OR IGNORE).
INSERT OR IGNORE INTO note_scripts (script_root, script_data) VALUES (?1, ?2)
