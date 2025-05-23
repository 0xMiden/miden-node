use miden_objects::note::{Note, NoteExecutionMode, NoteId, NoteMetadata, NoteTag, Nullifier};
use thiserror::Error;

/// A newtype that wraps around notes targeting a single account to be used in a network mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkNote(Note);

impl NetworkNote {
    pub fn inner(&self) -> &Note {
        &self.0
    }

    pub fn metadata(&self) -> &NoteMetadata {
        self.inner().metadata()
    }

    pub fn nullifier(&self) -> Nullifier {
        self.inner().nullifier()
    }

    pub fn id(&self) -> NoteId {
        self.inner().id()
    }
}

impl TryFrom<Note> for NetworkNote {
    type Error = NetworkNoteError;

    fn try_from(note: Note) -> Result<Self, Self::Error> {
        if !note.metadata().tag().is_single_target()
            || note.metadata().tag().execution_mode() != NoteExecutionMode::Network
        {
            return Ok(NetworkNote(note));
        }
        return Err(NetworkNoteError::InvalidExecutionMode(note.metadata().tag()));
    }
}

#[derive(Debug, Error)]
pub enum NetworkNoteError {
    #[error("note tag {0} is not a valid network note tag")]
    InvalidExecutionMode(NoteTag),
}
