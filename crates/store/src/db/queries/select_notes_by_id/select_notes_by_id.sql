-- Returns the notes with the given ids, including their details and script where stored.
--
-- The script lives in `note_scripts`, keyed by the note's script root; the join is outer because
-- private notes store no script.
SELECT notes.committed_at, notes.batch_index, notes.note_index, notes.note_id, notes.note_type,
       notes.sender, notes.tag, notes.attachment, notes.assets, notes.storage, notes.serial_num,
       notes.inclusion_path, note_scripts.script
FROM notes
LEFT JOIN note_scripts ON notes.script_root = note_scripts.script_root
WHERE notes.note_id IN (SELECT value FROM rarray(?1));
