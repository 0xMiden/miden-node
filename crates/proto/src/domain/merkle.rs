use miden_objects::{
    Word,
    crypto::merkle::{
        Forest, LeafIndex, MerklePath, MmrDelta, SmtLeaf, SmtProof, SparseMerklePath,
    },
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
        merkle_path.siblings.iter().map(Word::try_from).collect()
    }
}

impl TryFrom<proto::merkle::MerklePath> for MerklePath {
    type Error = ConversionError;

    fn try_from(merkle_path: proto::merkle::MerklePath) -> Result<Self, Self::Error> {
        (&merkle_path).try_into()
    }
}

// SPARSE MERKLE PATH
// ================================================================================================

impl From<SparseMerklePath> for proto::merkle::SparseMerklePath {
    fn from(value: SparseMerklePath) -> Self {
        let (empty_nodes_mask, siblings) = value.into_parts();
        proto::merkle::SparseMerklePath {
            empty_nodes_mask,
            siblings: siblings.into_iter().map(proto::digest::Digest::from).collect(),
        }
    }
}

impl TryFrom<proto::merkle::SparseMerklePath> for SparseMerklePath {
    type Error = ConversionError;

    fn try_from(merkle_path: proto::merkle::SparseMerklePath) -> Result<Self, Self::Error> {
        Ok(SparseMerklePath::from_parts(
            merkle_path.empty_nodes_mask,
            merkle_path
                .siblings
                .into_iter()
                .map(Word::try_from)
                .collect::<Result<Vec<_>, _>>()?,
        )?)
    }
}

// MMR DELTA
// ================================================================================================

impl From<MmrDelta> for proto::mmr::MmrDelta {
    fn from(value: MmrDelta) -> Self {
        let data = value.data.into_iter().map(proto::digest::Digest::from).collect();
        proto::mmr::MmrDelta {
            forest: value.forest.num_leaves() as u64,
            data,
        }
    }
}

impl TryFrom<proto::mmr::MmrDelta> for MmrDelta {
    type Error = ConversionError;

    fn try_from(value: proto::mmr::MmrDelta) -> Result<Self, Self::Error> {
        let data: Result<Vec<_>, ConversionError> =
            value.data.into_iter().map(Word::try_from).collect();

        Ok(MmrDelta {
            forest: Forest::new(value.forest as usize),
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
                let (key, value): (Word, Word) = entry.try_into()?;

                Ok(SmtLeaf::new_single(key, value))
            },
            proto::smt::smt_leaf::Leaf::Multiple(entries) => {
                let domain_entries: Vec<(Word, Word)> = try_convert(entries.entries)?;

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

impl TryFrom<proto::smt::SmtLeafEntry> for (Word, Word) {
    type Error = ConversionError;

    fn try_from(entry: proto::smt::SmtLeafEntry) -> Result<Self, Self::Error> {
        let key: Word = entry
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

impl From<(Word, Word)> for proto::smt::SmtLeafEntry {
    fn from((key, value): (Word, Word)) -> Self {
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
