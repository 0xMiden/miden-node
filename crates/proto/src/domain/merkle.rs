use miden_objects::{
    Digest, Word,
    crypto::merkle::{LeafIndex, MerklePath, MmrDelta, SmtLeaf, SmtProof},
};

use super::{convert, try_convert};
use crate::{
    errors::{ConversionError, MissingFieldHelper},
    generated::primitives as proto_primitives,
};

// MERKLE PATH
// ================================================================================================

impl From<&MerklePath> for proto_primitives::MerklePath {
    fn from(value: &MerklePath) -> Self {
        let siblings = value.nodes().iter().map(proto_primitives::Digest::from).collect();
        proto_primitives::MerklePath { siblings }
    }
}

impl From<MerklePath> for proto_primitives::MerklePath {
    fn from(value: MerklePath) -> Self {
        (&value).into()
    }
}

impl TryFrom<&proto_primitives::MerklePath> for MerklePath {
    type Error = ConversionError;

    fn try_from(merkle_path: &proto_primitives::MerklePath) -> Result<Self, Self::Error> {
        merkle_path.siblings.iter().map(Digest::try_from).collect()
    }
}

// MMR DELTA
// ================================================================================================

impl From<MmrDelta> for proto_primitives::MmrDelta {
    fn from(value: MmrDelta) -> Self {
        let data = value.data.into_iter().map(proto_primitives::Digest::from).collect();
        proto_primitives::MmrDelta { forest: value.forest as u64, data }
    }
}

impl TryFrom<proto_primitives::MmrDelta> for MmrDelta {
    type Error = ConversionError;

    fn try_from(value: proto_primitives::MmrDelta) -> Result<Self, Self::Error> {
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

impl TryFrom<proto_primitives::SmtLeaf> for SmtLeaf {
    type Error = ConversionError;

    fn try_from(value: proto_primitives::SmtLeaf) -> Result<Self, Self::Error> {
        let leaf = value.leaf.ok_or(proto_primitives::SmtLeaf::missing_field(stringify!(leaf)))?;

        match leaf {
            proto_primitives::smt_leaf::Leaf::Empty(leaf_index) => {
                Ok(Self::new_empty(LeafIndex::new_max_depth(leaf_index)))
            },
            proto_primitives::smt_leaf::Leaf::Single(entry) => {
                let (key, value): (Digest, Word) = entry.try_into()?;

                Ok(SmtLeaf::new_single(key, value))
            },
            proto_primitives::smt_leaf::Leaf::Multiple(entries) => {
                let domain_entries: Vec<(Digest, Word)> = try_convert(entries.entries)?;

                Ok(SmtLeaf::new_multiple(domain_entries)?)
            },
        }
    }
}

impl From<SmtLeaf> for proto_primitives::SmtLeaf {
    fn from(smt_leaf: SmtLeaf) -> Self {
        use proto_primitives::smt_leaf::Leaf;

        let leaf = match smt_leaf {
            SmtLeaf::Empty(leaf_index) => Leaf::Empty(leaf_index.value()),
            SmtLeaf::Single(entry) => Leaf::Single(entry.into()),
            SmtLeaf::Multiple(entries) => {
                Leaf::Multiple(proto_primitives::SmtLeafEntryList { entries: convert(entries) })
            },
        };

        Self { leaf: Some(leaf) }
    }
}

// SMT LEAF ENTRY
// ------------------------------------------------------------------------------------------------

impl TryFrom<proto_primitives::SmtLeafEntry> for (Digest, Word) {
    type Error = ConversionError;

    fn try_from(entry: proto_primitives::SmtLeafEntry) -> Result<Self, Self::Error> {
        let key: Digest = entry
            .key
            .ok_or(proto_primitives::SmtLeafEntry::missing_field(stringify!(key)))?
            .try_into()?;
        let value: Word = entry
            .value
            .ok_or(proto_primitives::SmtLeafEntry::missing_field(stringify!(value)))?
            .try_into()?;

        Ok((key, value))
    }
}

impl From<(Digest, Word)> for proto_primitives::SmtLeafEntry {
    fn from((key, value): (Digest, Word)) -> Self {
        Self {
            key: Some(key.into()),
            value: Some(value.into()),
        }
    }
}

// SMT PROOF
// ------------------------------------------------------------------------------------------------

impl TryFrom<proto_primitives::SmtOpening> for SmtProof {
    type Error = ConversionError;

    fn try_from(opening: proto_primitives::SmtOpening) -> Result<Self, Self::Error> {
        let path: MerklePath = opening
            .path
            .as_ref()
            .ok_or(proto_primitives::SmtOpening::missing_field(stringify!(path)))?
            .try_into()?;
        let leaf: SmtLeaf = opening
            .leaf
            .ok_or(proto_primitives::SmtOpening::missing_field(stringify!(leaf)))?
            .try_into()?;

        Ok(SmtProof::new(path, leaf)?)
    }
}

impl From<SmtProof> for proto_primitives::SmtOpening {
    fn from(proof: SmtProof) -> Self {
        let (ref path, leaf) = proof.into_parts();
        Self {
            path: Some(path.into()),
            leaf: Some(leaf.into()),
        }
    }
}
