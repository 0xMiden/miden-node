use std::collections::BTreeMap;

use miden_objects::{
    block::{BlockHeader, BlockInputs, NullifierWitness},
    note::{NoteId, NoteInclusionProof},
    transaction::PartialBlockchain,
    utils::{Deserializable, Serializable},
};

use crate::{
    AccountWitnessRecord, NullifierWitnessRecord,
    errors::{ConversionError, MissingFieldHelper},
    generated::{blockchain as blockchain_proto, note as note_proto, store as store_proto},
};

// BLOCK HEADER
// ================================================================================================

impl From<&BlockHeader> for blockchain_proto::BlockHeader {
    fn from(header: &BlockHeader) -> Self {
        Self {
            version: header.version(),
            prev_block_commitment: Some(header.prev_block_commitment().into()),
            block_num: header.block_num().as_u32(),
            chain_commitment: Some(header.chain_commitment().into()),
            account_root: Some(header.account_root().into()),
            nullifier_root: Some(header.nullifier_root().into()),
            note_root: Some(header.note_root().into()),
            tx_commitment: Some(header.tx_commitment().into()),
            tx_kernel_commitment: Some(header.tx_kernel_commitment().into()),
            proof_commitment: Some(header.proof_commitment().into()),
            timestamp: header.timestamp(),
        }
    }
}

impl From<BlockHeader> for blockchain_proto::BlockHeader {
    fn from(header: BlockHeader) -> Self {
        (&header).into()
    }
}

impl TryFrom<&blockchain_proto::BlockHeader> for BlockHeader {
    type Error = ConversionError;

    fn try_from(value: &blockchain_proto::BlockHeader) -> Result<Self, Self::Error> {
        value.try_into()
    }
}

impl TryFrom<blockchain_proto::BlockHeader> for BlockHeader {
    type Error = ConversionError;

    fn try_from(value: blockchain_proto::BlockHeader) -> Result<Self, Self::Error> {
        Ok(BlockHeader::new(
            value.version,
            value
                .prev_block_commitment
                .ok_or(blockchain_proto::BlockHeader::missing_field(stringify!(
                    prev_block_commitment
                )))?
                .try_into()?,
            value.block_num.into(),
            value
                .chain_commitment
                .ok_or(blockchain_proto::BlockHeader::missing_field(stringify!(chain_commitment)))?
                .try_into()?,
            value
                .account_root
                .ok_or(blockchain_proto::BlockHeader::missing_field(stringify!(account_root)))?
                .try_into()?,
            value
                .nullifier_root
                .ok_or(blockchain_proto::BlockHeader::missing_field(stringify!(nullifier_root)))?
                .try_into()?,
            value
                .note_root
                .ok_or(blockchain_proto::BlockHeader::missing_field(stringify!(note_root)))?
                .try_into()?,
            value
                .tx_commitment
                .ok_or(blockchain_proto::BlockHeader::missing_field(stringify!(tx_commitment)))?
                .try_into()?,
            value
                .tx_kernel_commitment
                .ok_or(blockchain_proto::BlockHeader::missing_field(stringify!(
                    tx_kernel_commitment
                )))?
                .try_into()?,
            value
                .proof_commitment
                .ok_or(blockchain_proto::BlockHeader::missing_field(stringify!(proof_commitment)))?
                .try_into()?,
            value.timestamp,
        ))
    }
}

// BLOCK INPUTS
// ================================================================================================

impl From<BlockInputs> for store_proto::BlockInputs {
    fn from(inputs: BlockInputs) -> Self {
        let (
            prev_block_header,
            partial_block_chain,
            account_witnesses,
            nullifier_witnesses,
            unauthenticated_note_proofs,
        ) = inputs.into_parts();

        store_proto::BlockInputs {
            latest_block_header: Some(prev_block_header.into()),
            account_witnesses: account_witnesses
                .into_iter()
                .map(|(id, witness)| AccountWitnessRecord { account_id: id, witness }.into())
                .collect(),
            nullifier_witnesses: nullifier_witnesses
                .into_iter()
                .map(|(nullifier, witness)| {
                    let proof = witness.into_proof();
                    NullifierWitnessRecord { nullifier, proof }.into()
                })
                .collect(),
            partial_block_chain: partial_block_chain.to_bytes(),
            unauthenticated_note_proofs: unauthenticated_note_proofs
                .iter()
                .map(note_proto::NoteInclusionInBlockProof::from)
                .collect(),
        }
    }
}

impl TryFrom<store_proto::BlockInputs> for BlockInputs {
    type Error = ConversionError;

    fn try_from(response: store_proto::BlockInputs) -> Result<Self, Self::Error> {
        let latest_block_header: BlockHeader = response
            .latest_block_header
            .ok_or(blockchain_proto::BlockHeader::missing_field("block_header"))?
            .try_into()?;

        let account_witnesses = response
            .account_witnesses
            .into_iter()
            .map(|entry| {
                let witness_record: AccountWitnessRecord = entry.try_into()?;
                Ok((witness_record.account_id, witness_record.witness))
            })
            .collect::<Result<BTreeMap<_, _>, ConversionError>>()?;

        let nullifier_witnesses = response
            .nullifier_witnesses
            .into_iter()
            .map(|entry| {
                let witness: NullifierWitnessRecord = entry.try_into()?;
                Ok((witness.nullifier, NullifierWitness::new(witness.proof)))
            })
            .collect::<Result<BTreeMap<_, _>, ConversionError>>()?;

        let unauthenticated_note_proofs = response
            .unauthenticated_note_proofs
            .iter()
            .map(<(NoteId, NoteInclusionProof)>::try_from)
            .collect::<Result<_, ConversionError>>()?;

        let partial_block_chain = PartialBlockchain::read_from_bytes(&response.partial_block_chain)
            .map_err(|source| {
                ConversionError::deserialization_error("PartialBlockchain", source)
            })?;

        Ok(BlockInputs::new(
            latest_block_header,
            partial_block_chain,
            account_witnesses,
            nullifier_witnesses,
            unauthenticated_note_proofs,
        ))
    }
}
