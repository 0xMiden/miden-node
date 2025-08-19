use std::collections::{HashMap, HashSet};

use miden_objects::Word;
use miden_objects::crypto::merkle::{
    Forest,
    InnerNode,
    LeafIndex,
    MerklePath,
    MmrDelta,
    NodeIndex,
    PartialSmt,
    SmtLeaf,
    SmtProof,
    SparseMerklePath,
};

use crate::domain::{convert, try_convert};
use crate::errors::{ConversionError, MissingFieldHelper};
use crate::generated as proto;

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
                .collect::<Result<Vec<_>, _>>()?,
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

impl TryFrom<proto::primitives::SmtLeaf> for SmtLeaf {
    type Error = ConversionError;

    fn try_from(value: proto::primitives::SmtLeaf) -> Result<Self, Self::Error> {
        let leaf = value.leaf.ok_or(proto::primitives::SmtLeaf::missing_field(stringify!(leaf)))?;

        match leaf {
            proto::primitives::smt_leaf::Leaf::EmptyLeafIndex(leaf_index) => {
                Ok(Self::new_empty(LeafIndex::new_max_depth(leaf_index)))
            },
            proto::primitives::smt_leaf::Leaf::Single(entry) => {
                let (key, value): (Word, Word) = entry.try_into()?;

                Ok(SmtLeaf::new_single(key, value))
            },
            proto::primitives::smt_leaf::Leaf::Multiple(entries) => {
                let domain_entries: Vec<(Word, Word)> =
                    try_convert(entries.entries).collect::<Result<_, _>>()?;

                Ok(SmtLeaf::new_multiple(domain_entries)?)
            },
        }
    }
}

impl From<SmtLeaf> for proto::primitives::SmtLeaf {
    fn from(smt_leaf: SmtLeaf) -> Self {
        use proto::primitives::smt_leaf::Leaf;

        let leaf = match smt_leaf {
            SmtLeaf::Empty(leaf_index) => Leaf::EmptyLeafIndex(leaf_index.value()),
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
        let key: Word = entry
            .key
            .ok_or(proto::primitives::SmtLeafEntry::missing_field(stringify!(key)))?
            .try_into()?;
        let value: Word = entry
            .value
            .ok_or(proto::primitives::SmtLeafEntry::missing_field(stringify!(value)))?
            .try_into()?;

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
        let path: MerklePath = opening
            .path
            .as_ref()
            .ok_or(proto::primitives::SmtOpening::missing_field(stringify!(path)))?
            .try_into()?;
        let leaf: SmtLeaf = opening
            .leaf
            .ok_or(proto::primitives::SmtOpening::missing_field(stringify!(leaf)))?
            .try_into()?;

        Ok(SmtProof::new(path, leaf)?)
    }
}

impl From<SmtProof> for proto::primitives::SmtOpening {
    fn from(proof: SmtProof) -> Self {
        let (ref path, leaf) = proof.into_parts();
        Self {
            path: Some(path.into()),
            leaf: Some(leaf.into()),
        }
    }
}

// NODE INDEX
// ------------------------------------------------------------------------------------------------
impl From<NodeIndex> for proto::primitives::NodeIndex {
    fn from(value: NodeIndex) -> Self {
        proto::primitives::NodeIndex {
            depth: value.depth() as u32,
            value: value.value(),
        }
    }
}
impl TryFrom<proto::primitives::NodeIndex> for NodeIndex {
    type Error = ConversionError;
    fn try_from(index: proto::primitives::NodeIndex) -> Result<Self, Self::Error> {
        let depth = u8::try_from(index.depth)?;
        let value = index.value;
        Ok(NodeIndex::new(depth, value)?)
    }
}

// PARTIAL SMT
// ------------------------------------------------------------------------------------------------

impl TryFrom<proto::primitives::PartialSmt> for PartialSmt {
    type Error = ConversionError;
    fn try_from(value: proto::primitives::PartialSmt) -> Result<Self, Self::Error> {
        let proto::primitives::PartialSmt { root, leaves, nodes } = value;
        let root = root
            .as_ref()
            .ok_or(proto::primitives::PartialSmt::missing_field(stringify!(root)))?
            .try_into()?;
        // TODO ensure `!leaves.is_empty()`

        // Convert other proto primitives to crypto types
        let leaves = Result::<Vec<SmtLeaf>, _>::from_iter(try_convert(leaves))?;
        let mut inner =
            Result::<HashMap<NodeIndex, Word>, _>::from_iter(nodes.into_iter().map(|inner| {
                let node_index = NodeIndex::try_from(
                    inner
                        .index
                        .ok_or(proto::primitives::NodeIndex::missing_field(stringify!(index)))?,
                )?;
                let digest = Word::try_from(
                    inner
                        .digest
                        .ok_or(proto::primitives::Digest::missing_field(stringify!(digest)))?,
                )?;
                Ok::<_, Self::Error>((node_index, digest))
            }))?;

        let leaf_indices =
            HashSet::<NodeIndex>::from_iter(leaves.iter().map(|leaf| leaf.index().into()));

        // Must contain the leaves too
        inner.extend(leaves.iter().map(|leaf| (leaf.index().into(), leaf.hash())));

        // Start constructing the partial SMT
        //
        // Construct a `MerklePath` per leaf by transcending from leaf digest down to depth 0.
        // Then verify the merkle proof holds consistency and completeness checks and all
        // required sibling nodes are present to deeduct required intermediate nodes.
        let mut partial = PartialSmt::new();
        for leaf in leaves {
            // Construct the merkle path:
            let leaf_node_index: NodeIndex = leaf.index().into();
            let mut current = leaf_node_index.clone();
            let mut siblings = Vec::new();

            // If we ever try to trancend beyond this depth level, something is wrong and
            // we must stop decoding.
            let max_depth = leaf_node_index.depth();
            // root:                00
            //                    /    \
            //                 10         11
            //                /  \       /  \
            //              20   21     22   23
            //              / \   / \  / \   / \
            //              leaves ...       x  y
            // Iterate from the leaf up to the root (exclusive)
            // We start by picking the sibling of `x`, `y`, our starting point and
            // then moving towards the root `0`. By definition siblings have the same parent.
            loop {
                let sibling_idx = current.sibling();
                // TODO FIXME for a leaf we get another leaf, we need to ensure those are part of
                // the inner set or contained in the inner HashMap
                let sibling_digest = if let Some(sibling_digest) = inner.get(&sibling_idx) {
                    // Previous round already calculated the entry or it was given explicitly
                    *sibling_digest
                } else {
                    // The entry does not exist, so we need to lazily follow the missing nodes and
                    // calculate recursively.

                    // DFS, build the subtree recursively, starting from the current sibling
                    let mut stack = Vec::<NodeIndex>::new();
                    stack.push(sibling_idx.clone());
                    loop {
                        let Some(idx) = stack.pop() else {
                            unreachable!(
                                "Must be an error, we must have nodes to resolve all questions, otherwise construction is borked"
                            )
                        };
                        if let Some(node_digest) = inner.get(&idx) {
                            if stack.is_empty() && idx == sibling_idx {
                                // we emptied the stack which means the current one is our desired
                                // starting point
                                break *node_digest;
                            }
                            // if the digest exists, we don't need to recurse
                            continue;
                        }
                        debug_assert!(
                            !leaf_indices.contains(&idx),
                            "For every relevant leaf, we must have the relevant value"
                        );
                        let left = idx.left_child();
                        let right = idx.right_child();
                        if max_depth < left.depth() || max_depth < right.depth() {
                            // TODO might happen in case of a missing node, so we must handle this
                            // gracefully
                            unreachable!("graceful!")
                        }
                        // proceed if the inner nodes are unknown
                        if !inner.contains_key(&left) {
                            stack.push(left);
                        }
                        if !inner.contains_key(&right) {
                            stack.push(right);
                        }
                        // left and right exist, we can derive the digest for `idx`
                        if let Some(&left) = inner.get(&left)
                            && let Some(&right) = inner.get(&right)
                        {
                            let node = InnerNode { left, right };
                            let node_digest = node.hash();

                            if stack.is_empty() && idx == sibling_idx {
                                // we emptied the stack which means the current one is our desired
                                // starting point
                                break node_digest;
                            }
                            inner.insert(idx, node_digest);
                        }
                    }
                };
                siblings.push(sibling_digest);

                // Move up to the parent level, and repeat
                current = current.parent();
                if current.depth() == 0 {
                    break;
                }
            }

            let path = MerklePath::new(siblings);
            path.verify(leaf_node_index.value(), leaf.hash(), &root).expect("It's fine");
            partial.add_path(leaf, path);
        }
        assert_eq!(partial.root(), root); // FIXME make error
        Ok(partial)
    }
}

impl From<PartialSmt> for proto::primitives::PartialSmt {
    fn from(partial: PartialSmt) -> Self {
        // Find all leaf digests, we need to include those, they are POIs
        let mut leaves = Vec::new();
        for (key, value) in partial.entries() {
            let leaf = partial.get_leaf(key).unwrap();
            leaves.push(crate::generated::primitives::SmtLeaf::from(leaf));
        }

        // Now collect the minimal set of internal nodes to be able to recalc the intermediate nodes
        // forming a partial smt
        let mut retained = HashMap::<NodeIndex, Word>::new();
        for (idx, node) in partial.inner_node_indices() {
            // if neither of the child keys are tracked, we cannot re-calc the inner node digest
            // on-the-fly and hence need to add the node to the set to be transferred
            if partial.get_value(node.left).is_err() || partial.get_value(node.left).is_err() {
                retained.insert(idx, node.hash());
                continue;
            }
        }
        let nodes = Vec::from_iter(retained.into_iter().map(|(index, digest)| {
            crate::generated::primitives::InnerNode {
                index: Some(crate::generated::primitives::NodeIndex::from(index)),
                digest: Some(crate::generated::primitives::Digest::from(digest)),
            }
        }));
        let root = Some(partial.root().into());
        // Remember: nodes and leaves as mutually exclusive
        Self { root, nodes, leaves }
    }
}

#[cfg(test)]
mod tests {
    use miden_objects::crypto::merkle::{PartialSmt, Smt};
    use pretty_assertions::assert_eq;

    use super::*;
    #[test]
    fn partial_smt_roundtrip() {
        let mut x = Smt::new();

        x.insert(Word::from([1_u32, 2, 3, 4]), Word::from([5_u32, 6, 7, 8]));
        x.insert(Word::from([10_u32, 11, 12, 13]), Word::from([14_u32, 15, 16, 17]));
        x.insert(Word::from([0x00_u32, 0xFF, 0xFF, 0xFF]), Word::from([0x00_u32; 4]));
        x.insert(Word::from([0xAA_u32, 0xFF, 0xFF, 0xFF]), Word::from([0xAA_u32; 4]));
        x.insert(Word::from([0xBB_u32, 0xFF, 0xFF, 0xFF]), Word::from([0xBB_u32; 4]));
        x.insert(Word::from([0xCC_u32, 0xFF, 0xFF, 0xFF]), Word::from([0xCC_u32; 4]));

        let proof = x.open(&Word::from([10_u32, 11, 12, 13]));

        let mut orig = PartialSmt::new();
        orig.add_proof(proof);
        let orig = orig;

        let proto = proto::primitives::PartialSmt::from(orig.clone());
        let recovered = PartialSmt::try_from(proto).unwrap();

        assert_eq!(orig, recovered);
    }
}
