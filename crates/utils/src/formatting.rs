use std::fmt::Display;

use itertools::Itertools;
use miden_protocol::transaction::{InputNoteCommitment, InputNotes, OutputNotes};

pub fn format_opt<T: Display>(opt: Option<&T>) -> String {
    opt.map_or("None".to_owned(), ToString::to_string)
}

pub fn format_input_notes(notes: &InputNotes<InputNoteCommitment>) -> String {
    format_array(notes.iter().map(|c| match c.header() {
        Some(header) => format!("({}, {})", c.nullifier().to_hex(), header.id().to_hex()),
        None => format!("({})", c.nullifier().to_hex()),
    }))
}

pub fn format_output_notes(notes: &OutputNotes) -> String {
    format_array(notes.iter().map(|output_note| {
        let metadata = output_note.metadata();
        format!(
            "{{ note_id: {}, note_metadata: {{sender: {}, tag: {} }}}}",
            output_note.id().to_hex(),
            metadata.sender(),
            metadata.tag(),
        )
    }))
}

pub fn format_array(list: impl IntoIterator<Item = impl Display>) -> String {
    let comma_separated = list.into_iter().join(", ");
    if comma_separated.is_empty() {
        "None".to_owned()
    } else {
        format!("[{comma_separated}]")
    }
}
