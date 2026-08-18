use std::collections::BTreeSet;

use miden_protocol::Word;
use miden_protocol::crypto::merkle::mmr::{Forest, MmrDelta};
use miden_protocol::crypto::merkle::smt::{
    LeafIndex,
    NodeValue,
    PartialSmt,
    SMT_DEPTH,
    SmtLeaf,
    SmtProof,
    UniqueNodes,
};
use miden_protocol::crypto::merkle::{MerklePath, NodeIndex, SparseMerklePath};

use crate::decode::{ConversionResultExt, GrpcDecodeExt};
use crate::domain::{convert, try_convert};
use crate::errors::ConversionError;
use crate::{decode, generated as proto};

// MERKLE PATH
// ================================================================================================

impl From<&MerklePath> for proto::primitives::MerklePath {
    fn from(value: &MerklePath) -> Self {
        let siblings = value.nodes().iter().map(proto::primitives::Digest::from).collect();
        proto::primitives::MerklePath { siblings }
    }
}

impl From<MerklePath> for proto::primitives::MerklePath {
    fn from(value: MerklePath) -> Self {
        (&value).into()
    }
}

impl TryFrom<&proto::primitives::MerklePath> for MerklePath {
    type Error = ConversionError;

    fn try_from(merkle_path: &proto::primitives::MerklePath) -> Result<Self, Self::Error> {
        merkle_path.siblings.iter().map(Word::try_from).collect()
    }
}

impl TryFrom<proto::primitives::MerklePath> for MerklePath {
    type Error = ConversionError;

    fn try_from(merkle_path: proto::primitives::MerklePath) -> Result<Self, Self::Error> {
        (&merkle_path).try_into()
    }
}

// SPARSE MERKLE PATH
// ================================================================================================

impl From<SparseMerklePath> for proto::primitives::SparseMerklePath {
    fn from(value: SparseMerklePath) -> Self {
        let (empty_nodes_mask, siblings) = value.into_parts();
        proto::primitives::SparseMerklePath {
            empty_nodes_mask,
            siblings: siblings.into_iter().map(proto::primitives::Digest::from).collect(),
        }
    }
}

impl TryFrom<proto::primitives::SparseMerklePath> for SparseMerklePath {
    type Error = ConversionError;

    fn try_from(merkle_path: proto::primitives::SparseMerklePath) -> Result<Self, Self::Error> {
        Ok(SparseMerklePath::from_parts(
            merkle_path.empty_nodes_mask,
            merkle_path
                .siblings
                .into_iter()
                .map(Word::try_from)
                .collect::<Result<Vec<_>, _>>()
                .context("siblings")?,
        )?)
    }
}

// MMR DELTA
// ================================================================================================

impl From<MmrDelta> for proto::primitives::MmrDelta {
    fn from(value: MmrDelta) -> Self {
        let data = value.data.into_iter().map(proto::primitives::Digest::from).collect();
        proto::primitives::MmrDelta {
            forest: value.forest.num_leaves() as u64,
            data,
        }
    }
}

impl TryFrom<proto::primitives::MmrDelta> for MmrDelta {
    type Error = ConversionError;

    fn try_from(value: proto::primitives::MmrDelta) -> Result<Self, Self::Error> {
        let data: Vec<_> = value
            .data
            .into_iter()
            .map(Word::try_from)
            .collect::<Result<_, _>>()
            .context("data")?;

        let forest_size: usize =
            value.forest.try_into().context("forest size does not fit in usize")?;
        let forest = Forest::new(forest_size).context("forest size out of range")?;

        Ok(MmrDelta { forest, data })
    }
}

// SPARSE MERKLE TREE
// ================================================================================================

// SMT LEAF
// ------------------------------------------------------------------------------------------------

impl TryFrom<proto::primitives::SmtLeaf> for SmtLeaf {
    type Error = ConversionError;

    fn try_from(value: proto::primitives::SmtLeaf) -> Result<Self, Self::Error> {
        let decoder = value.decoder();
        let leaf = decode!(decoder, value.leaf)?;

        match leaf {
            proto::primitives::smt_leaf::Leaf::EmptyLeafIndex(leaf_index) => {
                Ok(Self::new_empty(LeafIndex::new_max_depth(leaf_index)))
            },
            proto::primitives::smt_leaf::Leaf::Single(entry) => {
                let (key, value): (Word, Word) = entry.try_into().context("entry")?;

                Ok(SmtLeaf::new_single(key, value))
            },
            proto::primitives::smt_leaf::Leaf::Multiple(entries) => {
                let domain_entries: Vec<(Word, Word)> =
                    try_convert(entries.entries).collect::<Result<_, _>>().context("entries")?;

                Ok(SmtLeaf::new_multiple(domain_entries)?)
            },
        }
    }
}

impl From<SmtLeaf> for proto::primitives::SmtLeaf {
    fn from(smt_leaf: SmtLeaf) -> Self {
        use proto::primitives::smt_leaf::Leaf;

        let leaf = match smt_leaf {
            SmtLeaf::Empty(leaf_index) => Leaf::EmptyLeafIndex(leaf_index.position()),
            SmtLeaf::Single(entry) => Leaf::Single(entry.into()),
            SmtLeaf::Multiple(entries) => Leaf::Multiple(proto::primitives::SmtLeafEntryList {
                entries: convert(entries).collect(),
            }),
        };

        Self { leaf: Some(leaf) }
    }
}

// SMT LEAF ENTRY
// ------------------------------------------------------------------------------------------------

impl TryFrom<proto::primitives::SmtLeafEntry> for (Word, Word) {
    type Error = ConversionError;

    fn try_from(entry: proto::primitives::SmtLeafEntry) -> Result<Self, Self::Error> {
        let decoder = entry.decoder();
        let key: Word = decode!(decoder, entry.key)?;
        let value: Word = decode!(decoder, entry.value)?;

        Ok((key, value))
    }
}

impl From<(Word, Word)> for proto::primitives::SmtLeafEntry {
    fn from((key, value): (Word, Word)) -> Self {
        Self {
            key: Some(key.into()),
            value: Some(value.into()),
        }
    }
}

// SMT PROOF
// ------------------------------------------------------------------------------------------------

impl TryFrom<proto::primitives::SmtOpening> for SmtProof {
    type Error = ConversionError;

    fn try_from(opening: proto::primitives::SmtOpening) -> Result<Self, Self::Error> {
        let decoder = opening.decoder();
        let path: SparseMerklePath = decode!(decoder, opening.path)?;
        let leaf: SmtLeaf = decode!(decoder, opening.leaf)?;

        Ok(SmtProof::new(path, leaf)?)
    }
}

impl From<SmtProof> for proto::primitives::SmtOpening {
    fn from(proof: SmtProof) -> Self {
        let (path, leaf) = proof.into_parts();
        Self {
            path: Some(path.into()),
            leaf: Some(leaf.into()),
        }
    }
}

// PARTIAL SMT
// ------------------------------------------------------------------------------------------------

impl From<UniqueNodes> for proto::primitives::PartialSmt {
    fn from(unique_nodes: UniqueNodes) -> Self {
        use proto::primitives::partial_smt_node::Value;

        let UniqueNodes { root, nodes, leaves, value_only_leaves } = unique_nodes;

        let mut node_levels = nodes.into_iter().collect::<Vec<_>>();
        node_levels.sort_by_key(|(depth, _)| *depth);
        let node_levels = node_levels
            .into_iter()
            .map(|(depth, nodes)| {
                let nodes = nodes
                    .into_iter()
                    .map(|(index, value)| {
                        let value = match value {
                            NodeValue::EmptySubtreeRoot => Value::EmptySubtreeRoot(true),
                            NodeValue::Present(value) => Value::Digest(value.into()),
                        };
                        proto::primitives::PartialSmtNode { index, value: Some(value) }
                    })
                    .collect();

                proto::primitives::PartialSmtNodeLevel { depth: u32::from(depth), nodes }
            })
            .collect();

        let leaves = leaves
            .into_iter()
            .map(|(index, leaf)| proto::primitives::IndexedSmtLeaf {
                index,
                leaf: Some(leaf.into()),
            })
            .collect();

        let value_only_leaves = value_only_leaves
            .into_iter()
            .map(|(index, value)| proto::primitives::IndexedDigest {
                index,
                value: Some(value.into()),
            })
            .collect();

        Self {
            root: Some(root.into()),
            node_levels,
            leaves,
            value_only_leaves,
        }
    }
}

impl TryFrom<proto::primitives::PartialSmt> for UniqueNodes {
    type Error = ConversionError;

    fn try_from(value: proto::primitives::PartialSmt) -> Result<Self, Self::Error> {
        use proto::primitives::partial_smt_node::Value;

        let decoder = value.decoder();
        let proto::primitives::PartialSmt {
            root,
            node_levels,
            leaves,
            value_only_leaves,
        } = value;

        let root = decode!(decoder, root)?;

        let mut seen_depths = BTreeSet::new();
        let mut decoded_levels = Vec::with_capacity(node_levels.len());
        for level in node_levels {
            let depth = u8::try_from(level.depth).context("node_levels.depth")?;
            if depth == 0 || depth >= SMT_DEPTH {
                return Err(ConversionError::message(format!(
                    "partial SMT node depth {depth} must be in the range 1..{SMT_DEPTH}"
                )));
            }
            if !seen_depths.insert(depth) {
                return Err(ConversionError::message(format!(
                    "partial SMT contains duplicate node depth {depth}"
                )));
            }

            let mut seen_indices = BTreeSet::new();
            let mut decoded_nodes = Vec::with_capacity(level.nodes.len());
            for node in level.nodes {
                NodeIndex::new(depth, node.index).context("node_levels.nodes.index")?;
                if !seen_indices.insert(node.index) {
                    return Err(ConversionError::message(format!(
                        "partial SMT contains duplicate node index {} at depth {depth}",
                        node.index
                    )));
                }

                let node_value = match node.value.ok_or_else(|| {
                    ConversionError::missing_field::<proto::primitives::PartialSmtNode>("value")
                })? {
                    Value::Digest(value) => NodeValue::Present(value.try_into().context("digest")?),
                    Value::EmptySubtreeRoot(true) => NodeValue::EmptySubtreeRoot,
                    Value::EmptySubtreeRoot(false) => {
                        return Err(ConversionError::message(
                            "partial SMT empty_subtree_root marker must be true",
                        ));
                    },
                };
                decoded_nodes.push((node.index, node_value));
            }
            decoded_levels.push((depth, decoded_nodes));
        }

        let mut seen_leaf_indices = BTreeSet::new();
        let mut decoded_leaves = Vec::with_capacity(leaves.len());
        for indexed_leaf in leaves {
            if !seen_leaf_indices.insert(indexed_leaf.index) {
                return Err(ConversionError::message(format!(
                    "partial SMT contains duplicate leaf index {}",
                    indexed_leaf.index
                )));
            }
            let decoder = indexed_leaf.decoder();
            let leaf = decode!(decoder, indexed_leaf.leaf)?;
            decoded_leaves.push((indexed_leaf.index, leaf));
        }

        let mut seen_value_only_indices = BTreeSet::new();
        let mut decoded_value_only_leaves = Vec::with_capacity(value_only_leaves.len());
        for indexed_digest in value_only_leaves {
            if !seen_value_only_indices.insert(indexed_digest.index) {
                return Err(ConversionError::message(format!(
                    "partial SMT contains duplicate value-only leaf index {}",
                    indexed_digest.index
                )));
            }
            if seen_leaf_indices.contains(&indexed_digest.index) {
                return Err(ConversionError::message(format!(
                    "partial SMT leaf index {} has both a leaf and a value-only leaf",
                    indexed_digest.index
                )));
            }
            let decoder = indexed_digest.decoder();
            let digest = decode!(decoder, indexed_digest.value)?;
            decoded_value_only_leaves.push((indexed_digest.index, digest));
        }

        Ok(UniqueNodes {
            root,
            nodes: decoded_levels.into_iter().collect(),
            leaves: decoded_leaves,
            value_only_leaves: decoded_value_only_leaves,
        })
    }
}

impl From<PartialSmt> for proto::primitives::PartialSmt {
    fn from(partial_smt: PartialSmt) -> Self {
        partial_smt.to_unique_nodes().into()
    }
}

impl TryFrom<proto::primitives::PartialSmt> for PartialSmt {
    type Error = ConversionError;

    fn try_from(value: proto::primitives::PartialSmt) -> Result<Self, Self::Error> {
        let unique_nodes = UniqueNodes::try_from(value)?;
        PartialSmt::from_unique_nodes(unique_nodes)
            .map_err(|err| ConversionError::deserialization("PartialSmt", err))
    }
}

#[cfg(test)]
mod tests {
    use miden_protocol::crypto::merkle::smt::Smt;

    use super::*;

    #[test]
    fn partial_smt_round_trip() {
        let key0 = Word::from([1, 2, 3, 4u32]);
        let key1 = Word::from([5, 6, 7, 8u32]);
        let missing_key = Word::from([9, 10, 11, 12u32]);
        let value0 = Word::from([13, 14, 15, 16u32]);
        let value1 = Word::from([17, 18, 19, 20u32]);
        let smt = Smt::with_entries([(key0, value0), (key1, value1)]).unwrap();
        let partial_smt =
            PartialSmt::from_proofs([smt.open(&key0), smt.open(&missing_key)]).unwrap();

        let encoded: proto::primitives::PartialSmt = partial_smt.clone().into();
        assert!(encoded.node_levels.is_sorted_by_key(|level| level.depth));

        let decoded_unique_nodes = UniqueNodes::try_from(encoded).unwrap();
        let decoded = PartialSmt::from_unique_nodes(decoded_unique_nodes).unwrap();

        assert_eq!(decoded, partial_smt);
        assert_eq!(decoded.get_value(&key0).unwrap(), value0);
        assert_eq!(decoded.get_value(&missing_key).unwrap(), Word::empty());
    }

    #[test]
    fn partial_smt_rejects_false_empty_subtree_marker() {
        use proto::primitives::partial_smt_node::Value;

        let encoded = proto::primitives::PartialSmt {
            root: Some(PartialSmt::EMPTY_ROOT.into()),
            node_levels: vec![proto::primitives::PartialSmtNodeLevel {
                depth: 1,
                nodes: vec![proto::primitives::PartialSmtNode {
                    index: 0,
                    value: Some(Value::EmptySubtreeRoot(false)),
                }],
            }],
            leaves: vec![],
            value_only_leaves: vec![],
        };

        let err = UniqueNodes::try_from(encoded).unwrap_err();
        assert!(err.to_string().contains("must be true"));
    }

    #[test]
    fn partial_smt_rejects_duplicate_depths() {
        let encoded = proto::primitives::PartialSmt {
            root: Some(PartialSmt::EMPTY_ROOT.into()),
            node_levels: vec![
                proto::primitives::PartialSmtNodeLevel { depth: 1, nodes: vec![] },
                proto::primitives::PartialSmtNodeLevel { depth: 1, nodes: vec![] },
            ],
            leaves: vec![],
            value_only_leaves: vec![],
        };

        let err = UniqueNodes::try_from(encoded).unwrap_err();
        assert!(err.to_string().contains("duplicate node depth"));
    }

    #[test]
    fn partial_smt_rejects_invalid_node_index() {
        use proto::primitives::partial_smt_node::Value;

        let encoded = proto::primitives::PartialSmt {
            root: Some(PartialSmt::EMPTY_ROOT.into()),
            node_levels: vec![proto::primitives::PartialSmtNodeLevel {
                depth: 1,
                nodes: vec![proto::primitives::PartialSmtNode {
                    index: 2,
                    value: Some(Value::EmptySubtreeRoot(true)),
                }],
            }],
            leaves: vec![],
            value_only_leaves: vec![],
        };

        let err = UniqueNodes::try_from(encoded).unwrap_err();
        assert!(err.to_string().contains("not valid for depth"));
    }
}
