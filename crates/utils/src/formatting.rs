use std::fmt::Display;

use itertools::Itertools;
use miden_protocol::transaction::{InputNoteCommitment, InputNotes, OutputNotes};
use url::Url;

pub fn format_opt<T: Display>(opt: Option<&T>) -> String {
    opt.map_or("None".to_owned(), ToString::to_string)
}

pub fn format_input_notes(notes: &InputNotes<InputNoteCommitment>) -> String {
    format_array(notes.iter().map(|c| match c.header() {
        Some(header) => format!(
            "{{ nullifier: {}, note_id: {} }}",
            c.nullifier().to_hex(),
            header.id().to_hex()
        ),
        None => format!("{{ nullifier: {} }}", c.nullifier().to_hex()),
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

/// Formats a service endpoint without credentials, query parameters, or fragments.
pub fn format_endpoint(endpoint: &Url) -> String {
    let mut endpoint = endpoint.clone();
    let _ = endpoint.set_username("");
    let _ = endpoint.set_password(None);
    endpoint.set_query(None);
    endpoint.set_fragment(None);
    endpoint.to_string()
}

#[cfg(test)]
mod tests {
    use miden_protocol::Word;
    use miden_protocol::account::AccountId;
    use miden_protocol::note::{
        NoteAttachments, NoteDetailsCommitment, NoteHeader, NoteMetadata, NoteTag, NoteType,
        Nullifier, PartialNoteMetadata,
    };
    use miden_protocol::transaction::{InputNoteCommitment, InputNotes};
    use url::Url;

    use super::{format_endpoint, format_input_notes};

    #[test]
    fn input_notes_are_labeled() {
        let unresolved_nullifier = Nullifier::from_raw(Word::from([1, 2, 3, 4u32]));
        let resolved_nullifier = Nullifier::from_raw(Word::from([5, 6, 7, 8u32]));
        let sender = AccountId::try_from(0xfa00_0000_0000_bb01_0000_cc00_0000_de00_u128).unwrap();
        let header = NoteHeader::new(
            NoteDetailsCommitment::from_raw(Word::from([9, 10, 11, 12u32])),
            NoteMetadata::new(
                PartialNoteMetadata::new(sender, NoteType::Private).with_tag(NoteTag::new(1)),
                &NoteAttachments::default(),
            ),
        );
        let notes = InputNotes::new_unchecked(vec![
            InputNoteCommitment::from(unresolved_nullifier),
            InputNoteCommitment::from_parts_unchecked(resolved_nullifier, Some(header)),
        ]);

        assert_eq!(
            format_input_notes(&notes),
            format!(
                "[{{ nullifier: {} }}, {{ nullifier: {}, note_id: {} }}]",
                unresolved_nullifier.to_hex(),
                resolved_nullifier.to_hex(),
                header.id().to_hex(),
            ),
        );
    }

    #[test]
    fn endpoint_formatting_redacts_sensitive_parts() {
        let endpoint =
            Url::parse("https://user:secret@example.com:443/grpc?token=secret#fragment").unwrap();

        assert_eq!(format_endpoint(&endpoint), "https://example.com/grpc");
    }
}
