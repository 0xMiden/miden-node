use miden_objects::{
    Digest, Felt,
    block::BlockNumber,
    crypto::merkle::MerklePath,
    note::{NoteExecutionHint, NoteId, NoteInclusionProof, NoteMetadata, NoteTag, NoteType},
};

use crate::{
    errors::{ConversionError, MissingFieldHelper},
    generated::note as proto,
};

/// A note which has been committed to the chain and therefore has an associated index in a specific block and an inclusion proof.
#[derive(Debug, Clone, PartialEq)]
pub struct CommittedNote {
    pub block_num: BlockNumber,
    // TODO: can this be BlockNoteIndex? If so, how to convert to an absolute index.
    pub note_index: usize,
    pub note_id: Digest,
    pub metadata: NoteMetadata,
    pub details: Option<Vec<u8>>,
    pub merkle_path: MerklePath,
}

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

impl From<CommittedNote> for proto::Note {
    fn from(value: CommittedNote) -> Self {
        Self {
            block_num: value.block_num.as_u32(),
            note_index: value.note_index.try_into().expect("note index should fit in a u32"),
            note_id: Some(value.note_id.into()),
            metadata: Some(value.metadata.into()),
            merkle_path: Some(value.merkle_path.into()),
            details: value.details,
        }
    }
}
