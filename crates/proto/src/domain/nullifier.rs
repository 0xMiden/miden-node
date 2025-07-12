use miden_objects::{
    crypto::{hash::rpo::RpoDigest, merkle::SmtProof},
    note::Nullifier,
};

use crate::{
    errors::{ConversionError, MissingFieldHelper},
    generated::{primitives as proto_primitives, store as proto_store},
};

// FROM NULLIFIER
// ================================================================================================

impl From<&Nullifier> for proto_primitives::Digest {
    fn from(value: &Nullifier) -> Self {
        (*value).inner().into()
    }
}

impl From<Nullifier> for proto_primitives::Digest {
    fn from(value: Nullifier) -> Self {
        value.inner().into()
    }
}

// INTO NULLIFIER
// ================================================================================================

impl TryFrom<proto_primitives::Digest> for Nullifier {
    type Error = ConversionError;

    fn try_from(value: proto_primitives::Digest) -> Result<Self, Self::Error> {
        let digest: RpoDigest = value.try_into()?;
        Ok(digest.into())
    }
}

// NULLIFIER WITNESS RECORD
// ================================================================================================

#[derive(Clone, Debug)]
pub struct NullifierWitnessRecord {
    pub nullifier: Nullifier,
    pub proof: SmtProof,
}

impl TryFrom<proto_store::NullifierWitness> for NullifierWitnessRecord {
    type Error = ConversionError;

    fn try_from(
        nullifier_witness_record: proto_store::NullifierWitness,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            nullifier: nullifier_witness_record
                .nullifier
                .ok_or(proto_store::NullifierWitness::missing_field(stringify!(nullifier)))?
                .try_into()?,
            proof: nullifier_witness_record
                .opening
                .ok_or(proto_store::NullifierWitness::missing_field(stringify!(opening)))?
                .try_into()?,
        })
    }
}

impl From<NullifierWitnessRecord> for proto_store::NullifierWitness {
    fn from(value: NullifierWitnessRecord) -> Self {
        Self {
            nullifier: Some(value.nullifier.into()),
            opening: Some(value.proof.into()),
        }
    }
}
