use miden_objects::{
    crypto::{hash::rpo::RpoDigest, merkle::SmtProof},
    note::Nullifier,
};

use crate::{
    errors::{ConversionError, MissingFieldHelper},
    generated as proto,
};

// FROM NULLIFIER
// ================================================================================================

impl From<&Nullifier> for proto::primitives::Digest {
    fn from(value: &Nullifier) -> Self {
        (*value).inner().into()
    }
}

impl From<Nullifier> for proto::primitives::Digest {
    fn from(value: Nullifier) -> Self {
        value.inner().into()
    }
}

// INTO NULLIFIER
// ================================================================================================

impl TryFrom<proto::primitives::Digest> for Nullifier {
    type Error = ConversionError;

    fn try_from(value: proto::primitives::Digest) -> Result<Self, Self::Error> {
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

impl TryFrom<proto::block_producer_store::NullifierWitness> for NullifierWitnessRecord {
    type Error = ConversionError;

    fn try_from(
        nullifier_witness_record: proto::block_producer_store::NullifierWitness,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            nullifier: nullifier_witness_record
                .nullifier
                .ok_or(proto::block_producer_store::NullifierWitness::missing_field(stringify!(
                    nullifier
                )))?
                .try_into()?,
            proof: nullifier_witness_record
                .opening
                .ok_or(proto::block_producer_store::NullifierWitness::missing_field(stringify!(
                    opening
                )))?
                .try_into()?,
        })
    }
}

impl From<NullifierWitnessRecord> for proto::block_producer_store::NullifierWitness {
    fn from(value: NullifierWitnessRecord) -> Self {
        Self {
            nullifier: Some(value.nullifier.into()),
            opening: Some(value.proof.into()),
        }
    }
}
