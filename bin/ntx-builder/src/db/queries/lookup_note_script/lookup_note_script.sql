-- Looks up a note script by its root hash.
SELECT script_data FROM note_scripts WHERE script_root = ?1
