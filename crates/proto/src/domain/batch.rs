use std::collections::BTreeMap;

use miden_objects::{
    block::BlockHeader,
    note::{NoteId, NoteInclusionProof},
    transaction::PartialBlockchain,
    utils::{Deserializable, Serializable},
};

use crate::{
    errors::{ConversionError, MissingFieldHelper},
    generated::store as proto_store,
};

/// Data required for a transaction batch.
#[derive(Clone, Debug)]
pub struct BatchInputs {
    pub batch_reference_block_header: BlockHeader,
    pub note_proofs: BTreeMap<NoteId, NoteInclusionProof>,
    pub partial_block_chain: PartialBlockchain,
}

impl From<BatchInputs> for proto_store::BatchInputs {
    fn from(inputs: BatchInputs) -> Self {
        Self {
            batch_reference_block_header: Some(inputs.batch_reference_block_header.into()),
            note_proofs: inputs.note_proofs.iter().map(Into::into).collect(),
            partial_block_chain: inputs.partial_block_chain.to_bytes(),
        }
    }
}

impl TryFrom<proto_store::BatchInputs> for BatchInputs {
    type Error = ConversionError;

    fn try_from(response: proto_store::BatchInputs) -> Result<Self, ConversionError> {
        let result = Self {
            batch_reference_block_header: response
                .batch_reference_block_header
                .ok_or(proto_store::BatchInputs::missing_field("block_header"))?
                .try_into()?,
            note_proofs: response
                .note_proofs
                .iter()
                .map(<(NoteId, NoteInclusionProof)>::try_from)
                .collect::<Result<_, ConversionError>>()?,
            partial_block_chain: PartialBlockchain::read_from_bytes(&response.partial_block_chain)
                .map_err(|source| {
                    ConversionError::deserialization_error("PartialBlockchain", source)
                })?,
        };

        Ok(result)
    }
}
