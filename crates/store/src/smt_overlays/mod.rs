use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use miden_node_proto::generated::blockchain::Block;
use miden_objects::account::AccountId;
use miden_objects::block::{AccountTree, BlockNumber};
use miden_objects::crypto::merkle::{
    LeafIndex, MerklePath, MutationSet, NodeIndex, PartialSmt, Smt, SmtLeaf, SmtProof, SMT_DEPTH
};
use miden_objects::{Felt, FieldElement, Word, ZERO};

mod tests;

#[derive(thiserror::Error, Debug, PartialEq, Eq)]
#[error("fail: {0}")]
pub struct OverlayError(&'static str);

// essentially a MutationSet<SMT_DEPTH, Word, Word>;
#[derive(Debug, Clone)]
pub struct Overlay {
    old_root: Word,
    new_root: Word,
    // key to SmtLeaf (the hash of that is the value, I think)
    mutated: HashMap<Word, SmtLeaf>,

    // a lookup to see which intermediate nodes must be recalculated
    poisoned_tree_leaves: Vec<LeafIndex<SMT_DEPTH>>,
}

impl Overlay {
    // XXX scales linearly with `poisoned_tree_leaves`, but its all int-ops, so should be fairly fast
    fn is_part_of_poisoned_tree(&self, node_index: NodeIndex) -> bool {
        for &poisoned_tree_leaf in self.poisoned_tree_leaves.iter() {
            let mut poisoned_leaf_ancestor = NodeIndex::from(poisoned_tree_leaf);
            poisoned_leaf_ancestor.move_up_to(node_index.depth());
            if poisoned_leaf_ancestor == node_index {
                return true;
            }
        }
        false
    }
}

impl Overlay {
    /// Create the inversion of the given mutation
    pub fn inverted(
        current: &Smt,
        set: &MutationSet<SMT_DEPTH, Word, Word>,
    ) -> Result<Self, OverlayError> {
        let mut inverse_mutations = HashMap::new();

        for (key, _) in set.new_pairs() {
            inverse_mutations.insert(*key, current.get_leaf(key));
        }

        let poisoned_tree_leaves = Vec::from_iter(inverse_mutations.values().map(|leaf| leaf.index()));

        // Create and return the inverse mutation set
        Ok(Overlay {
            new_root: set.old_root(),
            old_root: set.root(),
            mutated: inverse_mutations,
            poisoned_tree_leaves: poisoned_tree_leaves,
        })
    }

    /// Root _pre_ applying the overlay
    pub fn root(&self) -> Word {
        self.new_root
    }

    /// Root _post_ applying the overlay
    pub fn old_root(&self) -> Word {
        self.old_root
    }
}

// impl From<MutationSet<SMT_DEPTH, Word, Word>> for Overlay {
//     fn from(value: MutationSet<SMT_DEPTH, Word, Word>) -> Self {
//         let mutated = HashMap::from_iter(value.new_pairs().map(|(key, _mutation)| {
//             (key, )
//         }));
//         Self {
//             old_root: value.old_root(),
//             new_root: value.root(),
//             mutated,
//         }
//     }
// }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoricalOffset {
    OverlayIdx(usize),
    Latest,
    TooAncient,
}

#[derive(Debug, Clone)]
pub struct SmtWithOverlays {
    /// The tip of the chain `AccountTree`
    latest: Smt,
    /// Overlays in order from latest to newest
    overlays: VecDeque<Overlay>,
    /// The block number of the oldest overlay
    base_block_number: BlockNumber,
}

impl SmtWithOverlays {
    const MAX_OVERLAYS: usize = 33;

    pub fn new(latest: Smt, base_block_number: BlockNumber) -> Self {
        Self {
            latest,
            overlays: VecDeque::new(),
            base_block_number,
        }
    }

    pub fn cleanup(&mut self) {
        while self.overlays.len() > Self::MAX_OVERLAYS {
            self.overlays.pop_back();
        }
    }

    pub fn oldest(&self) -> BlockNumber {
        self.base_block_number
    }

    // obtain the index on the in-memory overlays

    pub fn overlay_idx(latest: BlockNumber, requested: BlockNumber) -> HistoricalOffset {
        let Some(idx) = latest.as_u32().checked_sub(requested.as_u32()) else {
            return HistoricalOffset::TooAncient;
        };
        match idx {
            0 => HistoricalOffset::Latest,
            1..33 => HistoricalOffset::OverlayIdx(idx as usize - 1),
            _ => HistoricalOffset::TooAncient,
        }
    }

    /// Construct a new historical view on the account tree, if the relevant overlays are still available.
    pub fn historical_view<'a>(&'a self, height: BlockNumber) -> Option<HistoricalTreeView<'a>> {
        match Self::overlay_idx(self.base_block_number, height) {
            HistoricalOffset::Latest => Some(HistoricalTreeView { block_number: height, latest: &self.latest, stacks: (&[],&[]) } ),
            HistoricalOffset::OverlayIdx(idx) => {
                Some(HistoricalTreeView {
                    block_number: height,
                    latest: &self.latest, // FIXME Smt or AccountTree?
                    stacks: self.overlays.as_slices()
                })
            },
            HistoricalOffset::TooAncient => None,
        }
    }

    /// Add an overlay. Commonly called whenever we apply some new mutations to the _latest_ tree.
    pub fn add_overlay(&mut self, overlay: Overlay) {
        self.overlays.push_front(overlay);
        self.cleanup();
    }
}

///Pretend we were still at `block_number` of the `Smt`/`AccountTree`
struct HistoricalTreeView<'a> {
    block_number: BlockNumber,
    latest: &'a Smt,
    // 0 is top, nth is bottom, so if we query we query from the top
    stacks: (&'a [Overlay],&'a [Overlay]),
}

impl HistoricalTreeView<'_> {
    /// An overlay for the stacks
    fn overlay_iter<'b>(&'b self) -> impl Iterator<Item=&'b Overlay> {
        self.stacks.0.iter().chain(self.stacks.1.iter())
    }

    fn root(&self) -> Word {
        self.overlay_iter()
            .next()
            .map(|overlay| overlay.root())
            .unwrap_or_else(|| self.latest.root())
    }

    fn key_to_leaf_index(key: &Word) -> LeafIndex<SMT_DEPTH> {
        todo!()
    }

    /// Returns a [MerklePath] to the specified key.
    ///
    /// Mostly this is an implementation detail of [`Self::open()`].
    fn get_path(&self, key: &Word) -> MerklePath {
        let index = NodeIndex::from(Self::key_to_leaf_index(key));

        // proof indices include all siblings, so if we want to proof `x`
        //   root
        //    / \
        //  [f]   g
        //     /  \
        //    t   [q]
        //   / \
        //  x  [y]
        //
        //  proof: [y, q, f] (without root! and without the actual leaf!)
        //
        //
        // now we need a way to derive if we can use the `latest.get_node_hash(index)` or _any_ decendent got
        // updated, we are going to call this `is_part_of_poisoned_tree(idx)`


        pub type HashCache = HashMap::<NodeIndex, BTreeMap<BlockNumber,Word>>;

        // TODO: move this out elsewhere, to retain across get_path invocation
        let mut inner_node_hash_lut = HashMap::<NodeIndex, BTreeMap<BlockNumber,Word>>::new();

        // Dynamically calculate the cache, and and child hashes required to do so
        fn calc_dynamically(cache: &mut HashCache, index: NodexIndex, latest_afftected_block_num: BlockNumber) -> Word {
            inner_node_hash_lut.entry(index).or_default().entry(&latest_affected_block_num);

            let mut q = VecDeque::new();
            q.push()

            q.pop_front()
            // XXX
            //
            todo!()
        }

        MerklePath::from_iter(index
            .proof_indices()
            // iterates from leaves towards the root
            .map(|index| {
                // if any overlay poisons the current index, we have to calculate it again, the poisoning describes which `BlockNumber` we can use to do a cache lookup, since anything _later_ will not have poisoned the intermediate level
                let poisoning_set = HashSet::<BlockNumber>::new();
                // we know these are poisoned, so we skip all non-poisoned ones and do lookups from the last poisoning
                let poison_stack = Vec::from_iter(self.overlay_iter().map(|overlay| overlay.is_part_of_poisoned_tree(index)));

                if let Some(offset) = poison_stack.iter().position(|&x| x== true) {
                    let latest_affected_block_num = self.block_number.checked_sub(offset as u32).expect("By definition offset is at most 33 and cannot be larger than the number of blocks produced since genesis");

                    calc_dynamically(&mut cache, index, latest_affected_block_num);
                } else {

                    // none poisons the entry, we can use the pre-calculated one
                    self.latest.get_value(key)
                }

            })
            )
    }

    /// Get the hash of a node at an arbitrary index, including the root or leaf hashes.
    ///
    /// The root index simply returns [`Self::root()`]. Other hashes are retrieved by calling
    /// [`Self::get_inner_node()`] on the parent, and returning the respective child hash.
    fn get_node_hash(&self, index: NodeIndex) -> Word {

        if index.is_root() {
            return self.root();
        }

        let InnerNode { left, right } = self.smt.get_inner_node(index.parent());

        let index_is_right = index.is_value_odd();
        if index_is_right { right } else { left }
    }


    fn get_value(&self, key: &Word) -> Word {

        for overlay in self.overlay_iter() {
            if let Some(leaf) = overlay.mutated.get(key) {

                // Check if this leaf contains data for our key
                match leaf {
                    SmtLeaf::Single(entry) => {
                        // For single entries, return the value (second element of tuple)
                        return entry.1;
                    },
                    SmtLeaf::Multiple(entries) => {
                        // For multiple entries, find the matching key
                        for entry in entries {
                            if entry.0 == key {
                                return entry.1;
                            }
                        }
                    },
                    SmtLeaf::Empty(_) => {
                        // Empty leaf, continue searching
                    },
                }
            }
        }

        self.latest.get_value(key)
    }

    fn get_leaf(&self, key: &Word) -> SmtLeaf {
        for overlay in self.overlay_iter() {
            if let Some(leaf) = overlay.mutated.get(&key) {
                if !leaf.is_empty() {
                    return leaf.clone();
                }
            }
        }
        self.latest.get_leaf(key)
    }

    // XXX this is still a lot of hashing
    fn open(&self, key: &Word) -> SmtProof {
        let leaf = self.get_leaf(key);
        let leaf_idx = leaf.index();
        let lut = HashMap::from_iter();
        let y = leaf.index().proof_indices();

        let path = MerklePath::new(Vec::from_iter(idx.proof_indices().map(|idx| self.)));

        SmtProof::new(path, leaf).unwrap()
    }
}
