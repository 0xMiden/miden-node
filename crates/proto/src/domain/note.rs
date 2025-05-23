use miden_node_utils::note_tag::NetworkNote;
use miden_objects::{
    Digest, Felt,
    note::{
        Note, NoteDetails, NoteExecutionHint, NoteId, NoteInclusionProof, NoteMetadata, NoteTag,
        NoteType,
    },
    utils::{Deserializable, Serializable},
};

use crate::{
    errors::{ConversionError, MissingFieldHelper},
    generated::note as proto,
};

impl TryFrom<proto::NoteMetadata> for NoteMetadata {
    type Error = ConversionError;

    fn try_from(value: proto::NoteMetadata) -> Result<Self, Self::Error> {
        let sender = value
            .sender
            .ok_or_else(|| proto::NoteMetadata::missing_field(stringify!(sender)))?
            .try_into()?;
        let note_type = NoteType::try_from(u64::from(value.note_type))?;
        let tag = NoteTag::from(value.tag);

        let execution_hint = NoteExecutionHint::try_from(value.execution_hint)?;

        let aux = Felt::try_from(value.aux).map_err(|_| ConversionError::NotAValidFelt)?;

        Ok(NoteMetadata::new(sender, note_type, tag, execution_hint, aux)?)
    }
}

impl From<Note> for proto::NetworkNote {
    fn from(note: Note) -> Self {
        Self {
            metadata: Some(proto::NoteMetadata::from(*note.metadata())),
            details: NoteDetails::from(note).to_bytes(),
        }
    }
}

impl TryFrom<proto::NetworkNote> for NetworkNote {
    type Error = ConversionError;

    fn try_from(proto_note: proto::NetworkNote) -> Result<Self, Self::Error> {
        let details = NoteDetails::read_from_bytes(&proto_note.details)
            .map_err(|err| ConversionError::deserialization_error("NoteDetails", err))?;
        let (assets, recipient) = details.into_parts();

        let metadata: NoteMetadata = proto_note
            .metadata
            .ok_or_else(|| proto::NetworkNote::missing_field(stringify!(metadata)))?
            .try_into()?;
        let note = Note::new(assets, metadata, recipient);

        Ok(NetworkNote::try_from(note)?)
    }
}

impl From<NoteMetadata> for proto::NoteMetadata {
    fn from(val: NoteMetadata) -> Self {
        let sender = Some(val.sender().into());
        let note_type = val.note_type() as u32;
        let tag = val.tag().into();
        let execution_hint: u64 = val.execution_hint().into();
        let aux = val.aux().into();

        proto::NoteMetadata {
            sender,
            note_type,
            tag,
            execution_hint,
            aux,
        }
    }
}

impl From<(&NoteId, &NoteInclusionProof)> for proto::NoteInclusionInBlockProof {
    fn from((note_id, proof): (&NoteId, &NoteInclusionProof)) -> Self {
        Self {
            note_id: Some(note_id.into()),
            block_num: proof.location().block_num().as_u32(),
            note_index_in_block: proof.location().node_index_in_block().into(),
            merkle_path: Some(Into::into(proof.note_path())),
        }
    }
}

impl TryFrom<&proto::NoteInclusionInBlockProof> for (NoteId, NoteInclusionProof) {
    type Error = ConversionError;

    fn try_from(
        proof: &proto::NoteInclusionInBlockProof,
    ) -> Result<(NoteId, NoteInclusionProof), Self::Error> {
        Ok((
            Digest::try_from(
                proof
                    .note_id
                    .as_ref()
                    .ok_or(proto::NoteInclusionInBlockProof::missing_field(stringify!(note_id)))?,
            )?
            .into(),
            NoteInclusionProof::new(
                proof.block_num.into(),
                proof.note_index_in_block.try_into()?,
                proof
                    .merkle_path
                    .as_ref()
                    .ok_or(proto::NoteInclusionInBlockProof::missing_field(stringify!(
                        merkle_path
                    )))?
                    .try_into()?,
            )?,
        ))
    }
}
