// XXX NOT READY FOR REVIEW, just an outline and is missing some impl

use std::collections::{HashMap, VecDeque};

use miden_objects::{
    block::{AccountTree, BlockNumber}, crypto::merkle::{EmptySubtreeRoots, InnerNodeInfo, MerkleError, MerklePath, MutationSet, NodeIndex, PartialSmt, Smt, SmtLeaf, SmtProof, SMT_DEPTH}, AccountTreeError, Word, EMPTY_WORD
};

// essentially a MutationSet< 10u8, Word, Word>;
struct Overlay {
    old_root: Word,
    new_root: Word,
    mutated: HashMap<NodeIndex, SmtLeaf>,
}

impl Overlay {
    /// Create the inversion of the given mutation
    fn inverted(current: &AccountTree, set: &MutationSet<10u8, Word, Word>) -> Self {
        let mutations = todo!();
        // // Create a new mutation set that will contain the inverse operations
        // let mut inverse_mutations = HashMap::new();

        // // For each mutation, we need to create an inverse operation
        // // that will restore the original state when applied after the mutation
        // for (account_id, update) in mutations.iter() {
        //     // Get the current state of the account
        //     let current_hash = current.get(account_id);

        //     // The inverse mutation should restore the original hash
        //     // after the update has been applied
        //     inverse_mutations.push((account_id, current_hash));
        // }

        // Create and return the inverse mutation set
        Ok(Overlay {
            new_root: set.old_root(),
            old_root: set.new_root(),
            mutations,
        })
    }

    fn root(&self) -> Word {
        self.new_root
    }

    fn old_root(&self) -> Word {
        self.old_root
    }
}

impl From<MutationSet<10u8, Word, Word>> for Overlay {
fn from(value: MutationSet<10u8, Word, Word>) -> Self {
        let mutated = HashMap::from_iter(value.node_mutations().into_iter().inspect(|(_idx,_word)| { }).cloned());
        Self {
            old_root: value.old_root(),
            new_root: value.root(),
            mutated,
        }
    }
}

struct PipedTrie {
    /// The tip of the chain `AccountTree`
    latest: Smt,
    /// Overlays in order from latest to newest
    overlays: VecDeque<Overlay>,
}

impl PipedTrie {
    fn cleanup(&mut self) {
        todo!()
    }

    fn oldest(&self) -> BlockNumber {
        todo!()
    }

    fn at_height(&self, height: BlockNumber) ->  {

    }
}

// FIXME: expose this from `AccountTree`
pub(super) fn id_to_smt_key(account_id: AccountId) -> Word {
    // We construct this in such a way that we're forced to use the constants, so that when
    // they're updated, the other usages of the constants are also updated.
    let mut key = Word::empty();
    key[2] = account_id.suffix();
    key[3] = account_id.prefix().as_felt();

    key
}

/// Stacked trie
struct StackedTrie<'a> {
    block_number: BlockNumber,
    latest: &'a AccountTree,
    // 0 is top, nth is bottom, so if we query we query from the top
    stacks: &'a [Overlay],
}


impl StackedTrie<'_> {
    fn root(&self) -> Word {
        self.stacks.first().map(|overlay| overlay.root()).unwrap_or_else(|| return self.latest.root())
    }

    fn get_value(&self, key: &AccountId) -> Word {
        let key = id_to_smt_key(key);
        let idx = NodeIndex::from(key);
        for overlay in self.stacks.iter() {
            if let Some(value) = overlay.mutated.get(idx) {
                if key == value.hash() {
                    return value.hash()
                }
            }
        }
        self.latest.get(account_id)
    }

    fn get_leaf(&self, key: &AccountId) -> SmtLeaf {
        let key = id_to_smt_key(key);
        let idx = NodeIndex::from(key);
        for overlay in self.stacks.iter() {
            if let Some(value) = overlay.mutated.get(idx) {
                if key == value.hash() {
                    return value
                }
            }
        }
        self.latest.get(account_id)
    }

    fn open(&self, key: &Word) -> SmtProof {
        let idx = NodeIndex::from(key);
        let path = MerklePath::from_iter(idx.proof_indices().map(|x| {
            self.get_value()
        }));
        let leaf = self.get_value(); // FIXME
        SmtProof::new(path, leaf)
    }
}
