use miden_objects::note::{Note, NoteId, Nullifier};

/// A [`Note`] that is guaranteed to be a network note.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkNote(Note);

impl NetworkNote {
    pub fn new(note: Note) -> Option<Self> {
        note.is_network_note().then_some(Self::unchecked(note))
    }

    pub fn unchecked(note: Note) -> Self {
        Self(note)
    }

    pub fn inner(&self) -> &Note {
        &self.0
    }

    pub fn nullifier(&self) -> Nullifier {
        self.inner().nullifier()
    }

    pub fn is_single_target(&self) -> bool {
        self.inner().metadata().tag().is_single_target()
    }
}
