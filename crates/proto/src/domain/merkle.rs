use miden_objects::{
    crypto::merkle::{LeafIndex, MerklePath, MmrDelta, SmtLeaf, SmtProof},
    Digest, Word,
};

use super::{convert, try_convert};
use crate::{
    errors::{ConversionError, MissingFieldHelper},
    generated as proto,
};

// MERKLE PATH
// ================================================================================================

impl From<&MerklePath> for proto::merkle::MerklePath {
    fn from(value: &MerklePath) -> Self {
        let siblings = value.nodes().iter().map(proto::digest::Digest::from).collect();
        proto::merkle::MerklePath { siblings }
    }
}

impl From<MerklePath> for proto::merkle::MerklePath {
    fn from(value: MerklePath) -> Self {
        (&value).into()
    }
}

impl TryFrom<&proto::merkle::MerklePath> for MerklePath {
    type Error = ConversionError;

    fn try_from(merkle_path: &proto::merkle::MerklePath) -> Result<Self, Self::Error> {
        merkle_path.siblings.iter().map(Digest::try_from).collect()
    }
}

// MMR DELTA
// ================================================================================================

impl From<MmrDelta> for proto::mmr::MmrDelta {
    fn from(value: MmrDelta) -> Self {
        let data = value.data.into_iter().map(proto::digest::Digest::from).collect();
        proto::mmr::MmrDelta { forest: value.forest as u64, data }
    }
}

impl TryFrom<proto::mmr::MmrDelta> for MmrDelta {
    type Error = ConversionError;

    fn try_from(value: proto::mmr::MmrDelta) -> Result<Self, Self::Error> {
        let data: Result<Vec<_>, ConversionError> =
            value.data.into_iter().map(Digest::try_from).collect();

        Ok(MmrDelta {
            forest: value.forest as usize,
            data: data?,
        })
    }
}

// SPARSE MERKLE TREE
// ================================================================================================

// SMT LEAF
// ------------------------------------------------------------------------------------------------

impl TryFrom<proto::smt::SmtLeaf> for SmtLeaf {
    type Error = ConversionError;

    fn try_from(value: proto::smt::SmtLeaf) -> Result<Self, Self::Error> {
        let leaf = value.leaf.ok_or(proto::smt::SmtLeaf::missing_field(stringify!(leaf)))?;

        match leaf {
            proto::smt::smt_leaf::Leaf::Empty(leaf_index) => {
                Ok(Self::new_empty(LeafIndex::new_max_depth(leaf_index)))
            },
            proto::smt::smt_leaf::Leaf::Single(entry) => {
                let (key, value): (Digest, Word) = entry.try_into()?;

                Ok(SmtLeaf::new_single(key, value))
            },
            proto::smt::smt_leaf::Leaf::Multiple(entries) => {
                let domain_entries: Vec<(Digest, Word)> = try_convert(entries.entries)?;

                Ok(SmtLeaf::new_multiple(domain_entries)?)
            },
        }
    }
}

impl From<SmtLeaf> for proto::smt::SmtLeaf {
    fn from(smt_leaf: SmtLeaf) -> Self {
        use proto::smt::smt_leaf::Leaf;

        let leaf = match smt_leaf {
            SmtLeaf::Empty(leaf_index) => Leaf::Empty(leaf_index.value()),
            SmtLeaf::Single(entry) => Leaf::Single(entry.into()),
            SmtLeaf::Multiple(entries) => {
                Leaf::Multiple(proto::smt::SmtLeafEntries { entries: convert(entries) })
            },
        };

        Self { leaf: Some(leaf) }
    }
}

// SMT LEAF ENTRY
// ------------------------------------------------------------------------------------------------

impl TryFrom<proto::smt::SmtLeafEntry> for (Digest, Word) {
    type Error = ConversionError;

    fn try_from(entry: proto::smt::SmtLeafEntry) -> Result<Self, Self::Error> {
        let key: Digest = entry
            .key
            .ok_or(proto::smt::SmtLeafEntry::missing_field(stringify!(key)))?
            .try_into()?;
        let value: Word = entry
            .value
            .ok_or(proto::smt::SmtLeafEntry::missing_field(stringify!(value)))?
            .try_into()?;

        Ok((key, value))
    }
}

impl From<(Digest, Word)> for proto::smt::SmtLeafEntry {
    fn from((key, value): (Digest, Word)) -> Self {
        Self {
            key: Some(key.into()),
            value: Some(value.into()),
        }
    }
}

// SMT PROOF
// ------------------------------------------------------------------------------------------------

impl TryFrom<proto::smt::SmtOpening> for SmtProof {
    type Error = ConversionError;

    fn try_from(opening: proto::smt::SmtOpening) -> Result<Self, Self::Error> {
        let path: MerklePath = opening
            .path
            .as_ref()
            .ok_or(proto::smt::SmtOpening::missing_field(stringify!(path)))?
            .try_into()?;
        let leaf: SmtLeaf = opening
            .leaf
            .ok_or(proto::smt::SmtOpening::missing_field(stringify!(leaf)))?
            .try_into()?;

        Ok(SmtProof::new(path, leaf)?)
    }
}

impl From<SmtProof> for proto::smt::SmtOpening {
    fn from(proof: SmtProof) -> Self {
        let (ref path, leaf) = proof.into_parts();
        Self {
            path: Some(path.into()),
            leaf: Some(leaf.into()),
        }
    }
}
