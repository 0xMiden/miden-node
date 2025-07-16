use miden_objects::{Word, crypto::merkle::SmtProof, note::Nullifier};

use crate::{
    errors::{ConversionError, MissingFieldHelper},
    generated as proto,
};

// FROM NULLIFIER
// ================================================================================================

impl From<&Nullifier> for proto::digest::Digest {
    fn from(value: &Nullifier) -> Self {
        value.as_word().into()
    }
}

impl From<Nullifier> for proto::digest::Digest {
    fn from(value: Nullifier) -> Self {
        value.as_word().into()
    }
}

// INTO NULLIFIER
// ================================================================================================

impl TryFrom<proto::digest::Digest> for Nullifier {
    type Error = ConversionError;

    fn try_from(value: proto::digest::Digest) -> Result<Self, Self::Error> {
        let word: Word = value.try_into()?;
        Ok(word.into())
    }
}

// NULLIFIER WITNESS RECORD
// ================================================================================================

#[derive(Clone, Debug)]
pub struct NullifierWitnessRecord {
    pub nullifier: Nullifier,
    pub proof: SmtProof,
}

impl TryFrom<proto::responses::NullifierWitness> for NullifierWitnessRecord {
    type Error = ConversionError;

    fn try_from(
        nullifier_witness_record: proto::responses::NullifierWitness,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            nullifier: nullifier_witness_record
                .nullifier
                .ok_or(proto::responses::NullifierWitness::missing_field(stringify!(nullifier)))?
                .try_into()?,
            proof: nullifier_witness_record
                .opening
                .ok_or(proto::responses::NullifierWitness::missing_field(stringify!(opening)))?
                .try_into()?,
        })
    }
}

impl From<NullifierWitnessRecord> for proto::responses::NullifierWitness {
    fn from(value: NullifierWitnessRecord) -> Self {
        Self {
            nullifier: Some(value.nullifier.into()),
            opening: Some(value.proof.into()),
        }
    }
}
