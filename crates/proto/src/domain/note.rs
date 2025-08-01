use miden_objects::{
    Felt, Word,
    crypto::merkle::SparseMerklePath,
    note::{
        Note, NoteDetails, NoteExecutionHint, NoteId, NoteInclusionProof, NoteMetadata, NoteTag,
        NoteType, Nullifier,
    },
    utils::{Deserializable, Serializable},
};
use thiserror::Error;

use super::account::NetworkAccountPrefix;
use crate::{
    errors::{ConversionError, MissingFieldHelper},
    generated as proto,
};

impl TryFrom<proto::note::NoteMetadata> for NoteMetadata {
    type Error = ConversionError;

    fn try_from(value: proto::note::NoteMetadata) -> Result<Self, Self::Error> {
        let sender = value
            .sender
            .ok_or_else(|| proto::note::NoteMetadata::missing_field(stringify!(sender)))?
            .try_into()?;
        let note_type = NoteType::try_from(u64::from(value.note_type))?;
        let tag = NoteTag::from(value.tag);

        let execution_hint = NoteExecutionHint::try_from(value.execution_hint)?;

        let aux = Felt::try_from(value.aux).map_err(|_| ConversionError::NotAValidFelt)?;

        Ok(NoteMetadata::new(sender, note_type, tag, execution_hint, aux)?)
    }
}

impl From<Note> for proto::note::NetworkNote {
    fn from(note: Note) -> Self {
        Self {
            metadata: Some(proto::note::NoteMetadata::from(*note.metadata())),
            details: NoteDetails::from(note).to_bytes(),
        }
    }
}

impl From<Note> for proto::note::Note {
    fn from(note: Note) -> Self {
        Self {
            metadata: Some(proto::note::NoteMetadata::from(*note.metadata())),
            details: Some(NoteDetails::from(note).to_bytes()),
        }
    }
}

impl From<NetworkNote> for proto::note::NetworkNote {
    fn from(note: NetworkNote) -> Self {
        let note = Note::from(note);
        Self {
            metadata: Some(proto::note::NoteMetadata::from(*note.metadata())),
            details: NoteDetails::from(note).to_bytes(),
        }
    }
}

impl From<NoteMetadata> for proto::note::NoteMetadata {
    fn from(val: NoteMetadata) -> Self {
        let sender = Some(val.sender().into());
        let note_type = val.note_type() as u32;
        let tag = val.tag().into();
        let execution_hint: u64 = val.execution_hint().into();
        let aux = val.aux().into();

        proto::note::NoteMetadata {
            sender,
            note_type,
            tag,
            execution_hint,
            aux,
        }
    }
}

impl From<Word> for proto::note::NoteId {
    fn from(digest: Word) -> Self {
        Self { id: Some(digest.into()) }
    }
}

impl TryFrom<proto::note::NoteId> for Word {
    type Error = ConversionError;

    fn try_from(note_id: proto::note::NoteId) -> Result<Self, Self::Error> {
        note_id
            .id
            .as_ref()
            .ok_or(proto::note::NoteId::missing_field(stringify!(id)))?
            .try_into()
    }
}

impl From<&NoteId> for proto::note::NoteId {
    fn from(note_id: &NoteId) -> Self {
        Self { id: Some(note_id.into()) }
    }
}

impl From<(&NoteId, &NoteInclusionProof)> for proto::note::NoteInclusionInBlockProof {
    fn from((note_id, proof): (&NoteId, &NoteInclusionProof)) -> Self {
        Self {
            note_id: Some(note_id.into()),
            block_num: proof.location().block_num().as_u32(),
            note_index_in_block: proof.location().node_index_in_block().into(),
            inclusion_path: Some(proof.note_path().clone().into()),
        }
    }
}

impl TryFrom<&proto::note::NoteInclusionInBlockProof> for (NoteId, NoteInclusionProof) {
    type Error = ConversionError;

    fn try_from(
        proof: &proto::note::NoteInclusionInBlockProof,
    ) -> Result<(NoteId, NoteInclusionProof), Self::Error> {
        let inclusion_path = SparseMerklePath::try_from(
            proof
                .inclusion_path
                .as_ref()
                .ok_or(proto::note::NoteInclusionInBlockProof::missing_field(stringify!(
                    inclusion_path
                )))?
                .clone(),
        )?;

        Ok((
            Word::try_from(
                proof
                    .note_id
                    .as_ref()
                    .ok_or(proto::note::NoteInclusionInBlockProof::missing_field(stringify!(
                        note_id
                    )))?
                    .id
                    .as_ref()
                    .ok_or(proto::note::NoteId::missing_field(stringify!(id)))?,
            )?
            .into(),
            NoteInclusionProof::new(
                proof.block_num.into(),
                proof.note_index_in_block.try_into()?,
                inclusion_path,
            )?,
        ))
    }
}

impl TryFrom<proto::note::Note> for Note {
    type Error = ConversionError;

    fn try_from(proto_note: proto::note::Note) -> Result<Self, Self::Error> {
        let metadata: NoteMetadata = proto_note
            .metadata
            .ok_or(proto::note::Note::missing_field(stringify!(metadata)))?
            .try_into()?;

        let details = proto_note
            .details
            .ok_or(proto::note::Note::missing_field(stringify!(details)))?;

        let note_details = NoteDetails::read_from_bytes(&details)
            .map_err(|err| ConversionError::deserialization_error("NoteDetails", err))?;

        let (assets, recipient) = note_details.into_parts();
        Ok(Note::new(assets, metadata, recipient))
    }
}

// NETWORK NOTE
// ================================================================================================

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

    pub fn account_prefix(&self) -> NetworkAccountPrefix {
        // SAFETY: This must succeed because this is a network note.
        self.metadata().tag().try_into().unwrap()
    }
}

impl From<NetworkNote> for Note {
    fn from(value: NetworkNote) -> Self {
        value.0
    }
}

impl TryFrom<Note> for NetworkNote {
    type Error = NetworkNoteError;

    fn try_from(note: Note) -> Result<Self, Self::Error> {
        if !note.is_network_note() {
            return Err(NetworkNoteError::InvalidExecutionMode(note.metadata().tag()));
        }
        Ok(NetworkNote(note))
    }
}

#[derive(Debug, Error)]
pub enum NetworkNoteError {
    #[error("note tag {0} is not a valid network note tag")]
    InvalidExecutionMode(NoteTag),
}

impl TryFrom<proto::note::NetworkNote> for NetworkNote {
    type Error = ConversionError;

    fn try_from(proto_note: proto::note::NetworkNote) -> Result<Self, Self::Error> {
        let details = NoteDetails::read_from_bytes(&proto_note.details)
            .map_err(|err| ConversionError::deserialization_error("NoteDetails", err))?;
        let (assets, recipient) = details.into_parts();
        let metadata: NoteMetadata = proto_note
            .metadata
            .ok_or_else(|| proto::note::NetworkNote::missing_field(stringify!(metadata)))?
            .try_into()?;
        let note = Note::new(assets, metadata, recipient);

        NetworkNote::try_from(note).map_err(Into::into)
    }
}
