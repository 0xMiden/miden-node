//! Historical tracking for `AccountTree` via mutation overlays

use std::collections::VecDeque;
use std::sync::{Arc, RwLock};

use miden_objects::account::AccountId;
use miden_objects::block::{AccountMutationSet, AccountTree, AccountWitness, BlockNumber};
use miden_objects::crypto::merkle::{
    LargeSmt,
    LeafIndex,
    MemoryStorage,
    MerkleError,
    NodeIndex,
    NodeMutation,
    SMT_DEPTH,
    SmtLeaf,
    SmtStorage,
    SparseMerklePath,
};
use miden_objects::{AccountTreeError, EMPTY_WORD, Word};

#[cfg(test)]
mod tests;

#[allow(missing_docs)]
#[derive(thiserror::Error, Debug)]
pub enum HistoricalError {
    #[error(transparent)]
    MerkleError(#[from] MerkleError),
    #[error(transparent)]
    AccountTreeError(#[from] AccountTreeError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoricalOffset {
    Future,
    ReversionsIdx(usize),
    Latest,
    TooAncient,
}

/// Captures reversion state for historical queries.
#[derive(Debug, Clone)]
struct HistoricalOverlay {
    block_number: BlockNumber,
    rev_set: AccountMutationSet,
}

impl HistoricalOverlay {
    fn new(block_number: BlockNumber, rev_set: AccountMutationSet) -> Self {
        Self { block_number, rev_set }
    }
}

#[derive(Debug)]
struct InnerState<S = MemoryStorage>
where
    S: SmtStorage + Default,
{
    block_number: BlockNumber,
    latest: AccountTree<LargeSmt<S>>,
    overlays: VecDeque<Arc<HistoricalOverlay>>,
}

impl<S> InnerState<S>
where
    S: SmtStorage + Default,
{
    pub fn historical_offset(&self, desired_block_number: BlockNumber) -> HistoricalOffset {
        let Some(past_offset) = self.block_number.checked_sub(desired_block_number.as_u32()) else {
            return HistoricalOffset::Future;
        };
        let past_offset = past_offset.as_usize();
        match past_offset {
            0 => HistoricalOffset::Latest,
            1..=AccountTreeWithHistory::<S>::MAX_HISTORY => {
                HistoricalOffset::ReversionsIdx(past_offset - 1)
            },
            _ => HistoricalOffset::TooAncient,
        }
    }
}

/// Wraps `AccountTree` with historical query support via reversion overlays.
#[derive(Debug, Clone)]
pub struct AccountTreeWithHistory<S = MemoryStorage>
where
    S: SmtStorage + Default,
{
    inner: Arc<RwLock<InnerState<S>>>,
}

impl<S> AccountTreeWithHistory<S>
where
    S: SmtStorage + Default,
{
    pub const MAX_HISTORY: usize = 33;

    // CONSTRUCTORS
    // --------------------------------------------------------------------------------------------

    /// Creates a new historical tree.
    pub fn new(account_tree: AccountTree<LargeSmt<S>>, block_number: BlockNumber) -> Self {
        Self {
            inner: Arc::new(RwLock::new(InnerState {
                block_number,
                latest: account_tree,
                overlays: VecDeque::new(),
            })),
        }
    }

    fn cleanup(overlays: &mut VecDeque<Arc<HistoricalOverlay>>) {
        while overlays.len() > Self::MAX_HISTORY {
            overlays.pop_back();
        }
    }

    // PUBLIC ACCESSORS
    // --------------------------------------------------------------------------------------------

    /// Returns the latest block number.
    pub fn block_number(&self) -> BlockNumber {
        self.inner
            .read()
            .expect("RwLock poisoned: concurrent thread panicked while holding lock")
            .block_number
    }

    /// Returns the latest root.
    pub fn root(&self) -> Word {
        self.inner
            .read()
            .expect("RwLock poisoned: concurrent thread panicked while holding lock")
            .latest
            .root()
    }

    /// Returns the root at the given historical block.
    pub fn root_at(&self, block_number: BlockNumber) -> Option<Word> {
        let guard = self
            .inner
            .read()
            .expect("RwLock poisoned: concurrent thread panicked while holding lock");

        match guard.historical_offset(block_number) {
            HistoricalOffset::Latest => Some(guard.latest.root()),
            HistoricalOffset::ReversionsIdx(idx) => {
                let overlay_at = guard.overlays.get(idx)?;
                assert_eq!(overlay_at.block_number, block_number);
                Some(overlay_at.rev_set.as_mutation_set().root())
            },
            HistoricalOffset::Future | HistoricalOffset::TooAncient => None,
        }
    }

    /// Returns the account count.
    pub fn num_accounts(&self) -> usize {
        self.inner
            .read()
            .expect("RwLock poisoned: concurrent thread panicked while holding lock")
            .latest
            .num_accounts()
    }

    /// Returns the history depth.
    pub fn history_len(&self) -> usize {
        self.inner
            .read()
            .expect("RwLock poisoned: concurrent thread panicked while holding lock")
            .overlays
            .len()
    }

    /// Opens an account at the latest block.
    pub fn open(&self, account_id: AccountId) -> AccountWitness {
        self.inner
            .read()
            .expect("RwLock poisoned: concurrent thread panicked while holding lock")
            .latest
            .open(account_id)
    }

    /// Opens an account at a historical block.
    ///
    /// This method reconstructs the account witness at the given historical block by:
    /// 1. Starting with the latest account state
    /// 2. Applying reversion mutations from the overlays to walk back in time
    /// 3. Reconstructing the Merkle path with the historical node values
    pub fn open_at(
        &self,
        account_id: AccountId,
        block_number: BlockNumber,
    ) -> Option<AccountWitness> {
        let guard = self
            .inner
            .read()
            .expect("RwLock poisoned: concurrent thread panicked while holding lock");

        match guard.historical_offset(block_number) {
            HistoricalOffset::Latest => Some(guard.latest.open(account_id)),
            HistoricalOffset::ReversionsIdx(idx) => {
                if idx >= guard.overlays.len() {
                    return None;
                }

                Self::reconstruct_historical_witness(&guard, account_id, idx)
            },
            HistoricalOffset::Future | HistoricalOffset::TooAncient => None,
        }
    }

    /// Reconstructs a historical account witness by applying reversion overlays.
    fn reconstruct_historical_witness(
        guard: &InnerState<S>,
        account_id: AccountId,
        overlay_idx: usize,
    ) -> Option<AccountWitness> {
        let latest_witness = guard.latest.open(account_id);
        let (latest_path, mut leaf) = latest_witness.into_proof().into_parts();
        let (initial_mask, mut latest_nodes) = latest_path.into_parts();

        // Initialize path_nodes with the latest state.
        // Reverse latest_nodes because they come in high-to-low depth order
        // but we need to initialize path_nodes in low-to-high depth order.
        latest_nodes.reverse();
        let mut path_nodes = Self::initialize_path_nodes(initial_mask, &latest_nodes);

        let leaf_index = NodeIndex::from(leaf.index());

        // Apply reversion overlays to reconstruct the historical state.
        leaf = Self::apply_reversion_overlays(
            &guard.overlays,
            overlay_idx,
            &mut path_nodes,
            leaf_index,
            leaf,
        );

        // Reconstruct the SparseMerklePath from the historical path_nodes.
        let (empty_mask, nodes) = Self::build_sparse_path(&path_nodes);
        let path = SparseMerklePath::from_parts(empty_mask, nodes).ok()?;

        let commitment = match leaf {
            SmtLeaf::Empty(_) => EMPTY_WORD,
            SmtLeaf::Single((_, value)) => value,
            SmtLeaf::Multiple(_) => unreachable!("AccountTree uses prefix-free IDs"),
        };

        AccountWitness::new(account_id, commitment, path).ok()
    }

    /// Initializes the `path_nodes` array from the latest state.
    ///
    /// The `initial_mask` indicates which depths have empty nodes (bit set = empty).
    /// For non-empty depths, we populate from `latest_nodes`.
    fn initialize_path_nodes(
        initial_mask: u64,
        latest_nodes: &[Word],
    ) -> [Option<Word>; SMT_DEPTH as usize] {
        let mut path_nodes = [None; SMT_DEPTH as usize];
        let mut node_idx = 0;

        for (depth, path_node) in path_nodes.iter_mut().enumerate().take(SMT_DEPTH as usize) {
            // Check if this depth is non-empty in the initial mask.
            // If the bit at position `depth` is 0, the node is present; if 1, it's empty.
            if (initial_mask & (1u64 << depth)) == 0 && node_idx < latest_nodes.len() {
                *path_node = Some(latest_nodes[node_idx]);
                node_idx += 1;
            }
        }

        path_nodes
    }

    /// Applies reversion overlays to reconstruct the historical state.
    ///
    /// We iterate through overlays from newest to oldest (going back in time),
    /// updating both the path nodes and the leaf value based on the reversion mutations.
    fn apply_reversion_overlays(
        overlays: &VecDeque<Arc<HistoricalOverlay>>,
        overlay_idx: usize,
        path_nodes: &mut [Option<Word>; SMT_DEPTH as usize],
        leaf_index: NodeIndex,
        mut leaf: SmtLeaf,
    ) -> SmtLeaf {
        for overlay in overlays.iter().take(overlay_idx + 1) {
            let rev_muts = overlay.rev_set.as_mutation_set().node_mutations();

            // Update the path sibling nodes that changed in this overlay.
            for sibling in leaf_index.proof_indices() {
                let height =
                    sibling.depth().checked_sub(1).expect("proof_indices should not include root")
                        as usize;

                // Apply reversion mutations for this sibling node.
                if let Some(mutation) = rev_muts.get(&sibling) {
                    match mutation {
                        NodeMutation::Addition(inner_node) => {
                            path_nodes[height] = Some(inner_node.hash());
                        },
                        NodeMutation::Removal => {
                            path_nodes[height] = None;
                        },
                    }
                }
                // If no mutation exists, the node remains unchanged (already set from latest).
            }

            // Update the leaf if it was modified in this overlay.
            if let Some((key, value)) = overlay
                .rev_set
                .as_mutation_set()
                .new_pairs()
                .iter()
                .find(|(k, _)| LeafIndex::from(**k) == leaf.index())
            {
                leaf = if *value == EMPTY_WORD {
                    SmtLeaf::new_empty(leaf.index())
                } else {
                    SmtLeaf::new_single(*key, *value)
                };
            }
        }

        leaf
    }

    /// Builds a `SparseMerklePath` from the `path_nodes` array.
    ///
    /// The `empty_mask` is constructed by setting a bit for each empty node.
    /// We iterate from depth 0 to `max_depth` in reverse order (high to low)
    /// to build the nodes vector as expected by `SparseMerklePath`.
    fn build_sparse_path(path_nodes: &[Option<Word>; SMT_DEPTH as usize]) -> (u64, Vec<Word>) {
        let max_depth =
            path_nodes.iter().rposition(std::option::Option::is_some).map_or(0, |d| d + 1);

        // Start with all bits set (all empty), then clear bits for non-empty nodes.
        let mut empty_mask = u64::MAX;
        let nodes: Vec<Word> = (0..max_depth)
            .rev()
            .filter_map(|d| {
                path_nodes[d].inspect(|_| {
                    // Clear the bit at position `d` to indicate this node is present.
                    empty_mask &= !(1u64 << d);
                })
            })
            .collect();

        (empty_mask, nodes)
    }

    /// Gets the account state at the latest block.
    pub fn get(&self, account_id: AccountId) -> Word {
        self.inner
            .read()
            .expect("RwLock poisoned: concurrent thread panicked while holding lock")
            .latest
            .get(account_id)
    }

    // PUBLIC MUTATORS
    // --------------------------------------------------------------------------------------------

    /// Applies mutations and advances to the next block.
    pub fn apply_mutations(
        &self,
        account_commitments: impl IntoIterator<Item = (AccountId, Word)>,
    ) -> Result<(), HistoricalError> {
        let mut inner = self
            .inner
            .write()
            .expect("RwLock poisoned: concurrent thread panicked while holding lock");

        let accounts = Vec::from_iter(account_commitments);

        let mutations = inner.latest.compute_mutations(accounts)?;
        let rev = inner.latest.apply_mutations_with_reversion(mutations)?;
        let overlay = HistoricalOverlay::new(inner.block_number, rev);

        inner.overlays.push_front(Arc::new(overlay));
        inner.block_number = inner.block_number.child();
        Self::cleanup(&mut inner.overlays);

        Ok(())
    }

    /// Returns the oldest block still in history.
    pub fn oldest_block_num(&self) -> BlockNumber {
        let inner = self
            .inner
            .read()
            .expect("RwLock poisoned: concurrent thread panicked while holding lock");
        inner
            .block_number
            .checked_sub(inner.overlays.len() as u32)
            .unwrap_or(BlockNumber::GENESIS)
    }

    /// Checks if the tree contains the account prefix.
    pub fn contains_account_id_prefix(
        &self,
        prefix: miden_objects::account::AccountIdPrefix,
    ) -> bool {
        self.inner
            .read()
            .expect("RwLock poisoned: concurrent thread panicked while holding lock")
            .latest
            .contains_account_id_prefix(prefix)
    }
}
