//! Historical tracking for `AccountTree` via mutation overlays

use std::collections::BTreeMap;

use miden_crypto::merkle::{EmptySubtreeRoots, MerklePath};
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

/// Convenience for an in-memory-only account tree.
pub type InMemoryAccountTree = AccountTree<LargeSmt<MemoryStorage>>;

/// Trait abstracting operations over different account tree backends.
pub trait AccountTreeStorage {
    /// Returns the root hash of the tree.
    fn root(&self) -> Word;

    /// Returns the number of accounts in the tree.
    fn num_accounts(&self) -> usize;

    /// Opens an account and returns its witness.
    fn open(&self, account_id: AccountId) -> AccountWitness;

    /// Gets the account state commitment.
    fn get(&self, account_id: AccountId) -> Word;

    /// Computes mutations for applying account updates.
    fn compute_mutations(
        &self,
        accounts: impl IntoIterator<Item = (AccountId, Word)>,
    ) -> Result<AccountMutationSet, AccountTreeError>;

    /// Applies mutations with reversion data.
    fn apply_mutations_with_reversion(
        &mut self,
        mutations: AccountMutationSet,
    ) -> Result<AccountMutationSet, AccountTreeError>;

    /// Checks if the tree contains an account with the given prefix.
    fn contains_account_id_prefix(&self, prefix: miden_objects::account::AccountIdPrefix) -> bool;
}

impl<S> AccountTreeStorage for AccountTree<LargeSmt<S>>
where
    S: SmtStorage,
{
    fn root(&self) -> Word {
        self.root()
    }

    fn num_accounts(&self) -> usize {
        self.num_accounts()
    }

    fn open(&self, account_id: AccountId) -> AccountWitness {
        self.open(account_id)
    }

    fn get(&self, account_id: AccountId) -> Word {
        self.get(account_id)
    }

    fn compute_mutations(
        &self,
        accounts: impl IntoIterator<Item = (AccountId, Word)>,
    ) -> Result<AccountMutationSet, AccountTreeError> {
        self.compute_mutations(accounts)
    }

    fn apply_mutations_with_reversion(
        &mut self,
        mutations: AccountMutationSet,
    ) -> Result<AccountMutationSet, AccountTreeError> {
        self.apply_mutations_with_reversion(mutations)
    }

    fn contains_account_id_prefix(&self, prefix: miden_objects::account::AccountIdPrefix) -> bool {
        self.contains_account_id_prefix(prefix)
    }
}

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
pub enum HistoricalState {
    Future,
    Target(BlockNumber),
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

/// Wraps `AccountTree` with historical query support via reversion overlays.
#[derive(Debug, Clone)]
pub struct AccountTreeWithHistory<S>
where
    S: AccountTreeStorage,
{
    block_number: BlockNumber,
    latest: S,
    overlays: BTreeMap<BlockNumber, HistoricalOverlay>,
}

impl<S> AccountTreeWithHistory<S>
where
    S: AccountTreeStorage,
{
    pub const MAX_HISTORY: usize = 33;

    // CONSTRUCTORS
    // --------------------------------------------------------------------------------------------

    /// Creates a new historical tree.
    pub fn new(account_tree: S, block_number: BlockNumber) -> Self {
        Self {
            block_number,
            latest: account_tree,
            overlays: BTreeMap::new(),
        }
    }

    /// Remove any items that exceed the maximum number of historical overlays.
    fn drain_excess(overlays: &mut BTreeMap<BlockNumber, HistoricalOverlay>) {
        while overlays.len() > Self::MAX_HISTORY {
            overlays.pop_first();
        }
    }

    // PUBLIC ACCESSORS
    // --------------------------------------------------------------------------------------------

    /// Returns the latest block number.
    pub fn block_number_latest(&self) -> BlockNumber {
        self.block_number
    }

    /// Returns the latest root.
    pub fn root_latest(&self) -> Word {
        self.latest.root()
    }

    /// Returns the root at the given historical block.
    pub fn root_at(&self, block_number: BlockNumber) -> Option<Word> {
        match self.historical_state(block_number) {
            HistoricalState::Latest => Some(self.latest.root()),
            HistoricalState::Target(block_number) => {
                let overlay_at = self.overlays.get(&block_number)?;
                assert_eq!(overlay_at.block_number, block_number);
                Some(overlay_at.rev_set.as_mutation_set().root())
            },
            HistoricalState::Future | HistoricalState::TooAncient => None,
        }
    }

    /// Returns the account count.
    pub fn num_accounts_latest(&self) -> usize {
        self.latest.num_accounts()
    }

    /// Returns the history depth.
    pub fn history_len(&self) -> usize {
        self.overlays.len()
    }

    /// Opens an account at the latest block.
    pub fn open_latest(&self, account_id: AccountId) -> AccountWitness {
        self.latest.open(account_id)
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
        match self.historical_state(block_number) {
            HistoricalState::Latest => Some(self.latest.open(account_id)),
            HistoricalState::Target(block_number) => {
                self.overlays.get(&block_number)?;

                Self::reconstruct_historical_witness(&self, account_id, block_number)
            },
            HistoricalState::Future | HistoricalState::TooAncient => None,
        }
    }

    /// Checks if the tree contains the account prefix.
    pub fn contains_account_id_prefix(
        &self,
        prefix: miden_objects::account::AccountIdPrefix,
    ) -> bool {
        self.latest.contains_account_id_prefix(prefix)
    }

    pub fn historical_state(&self, desired_block_number: BlockNumber) -> HistoricalState {
        if desired_block_number == self.block_number {
            return HistoricalState::Latest;
        }
        let Some(_) = self.block_number.checked_sub(desired_block_number.as_u32()) else {
            return HistoricalState::Future;
        };
        let Some(_) = self.overlays.get(&desired_block_number) else {
            return HistoricalState::TooAncient;
        };
        HistoricalState::Target(desired_block_number)
    }

    // UTILTIES
    // --------------------------------------------------------------------------------------------

    /// Reconstructs a historical account witness by applying reversion overlays.
    fn reconstruct_historical_witness(
        &self,
        account_id: AccountId,
        block_target: BlockNumber,
    ) -> Option<AccountWitness> {
        let latest_witness = self.latest.open(account_id);
        let (latest_path, leaf) = latest_witness.into_proof().into_parts();
        let (initial_mask, mut latest_nodes) = latest_path.into_parts();

        // Initialize path_nodes with the latest state.
        // Reverse latest_nodes because they come in high-to-low depth order
        // but we need to initialize path_nodes in low-to-high depth order.
        latest_nodes.reverse();
        let path_nodes = Self::initialize_path_nodes(initial_mask, &latest_nodes);

        let leaf_index = NodeIndex::from(leaf.index());

        // Apply reversion overlays to reconstruct the historical state.
        let (path, leaf) = Self::apply_reversion_overlays(
            &self.overlays,
            block_target,
            path_nodes,
            leaf_index,
            leaf,
        )?;

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
        overlays: &BTreeMap<BlockNumber, HistoricalOverlay>,
        block_target: BlockNumber,
        mut path_nodes: [Option<Word>; SMT_DEPTH as usize],
        leaf_index: NodeIndex,
        mut leaf: SmtLeaf,
    ) -> Option<(SparseMerklePath, SmtLeaf)> {
        for (_, overlay) in
            overlays.iter().rev().take_while(|(block_num, _)| block_target <= **block_num)
        {
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

        let path = Self::build_dense_path(&path_nodes)?;
        let path = SparseMerklePath::try_from(path).ok()?;
        Some((path, leaf))
    }

    /// Builds a `SparseMerklePath` from the `path_nodes` array.
    ///
    /// The `empty_mask` is constructed by setting a bit for each empty node.
    /// We iterate from depth 0 to `max_depth` in reverse order (high to low)
    /// to build the nodes vector as expected by `SparseMerklePath`.
    fn build_dense_path(path_nodes: &[Option<Word>; SMT_DEPTH as usize]) -> Option<MerklePath> {
        // Start with all bits set (all empty), then clear bits for non-empty nodes.
        let dense: Vec<Word> = (0..SMT_DEPTH)
            .rev()
            .map(|d| {
                path_nodes[d as usize]
                    .as_ref()
                    .unwrap_or_else(|| EmptySubtreeRoots::entry(SMT_DEPTH, d + 1))
                    .clone()
            })
            .collect();

        let dense = MerklePath::new(dense);
        Some(dense)
    }

    /// Gets the account state at the latest block.
    pub fn get(&self, account_id: AccountId) -> Word {
        self.latest.get(account_id)
    }

    // PUBLIC MUTATORS
    // --------------------------------------------------------------------------------------------

    /// Combine [`compute_mutations`] and [`apply_mutations`].
    ///
    /// Primarily targeting testing.
    pub fn compute_and_apply_mutations(
        &mut self,
        account_commitments: impl IntoIterator<Item = (AccountId, Word)>,
    ) -> Result<(), HistoricalError> {
        let mutations = self.compute_mutations(account_commitments)?;
        self.apply_mutations(mutations)
    }

    /// Compute mutations relativ to the latest state.
    pub fn compute_mutations(
        &mut self,
        account_commitments: impl IntoIterator<Item = (AccountId, Word)>,
    ) -> Result<AccountMutationSet, HistoricalError> {
        Ok(self.latest.compute_mutations(account_commitments)?)
    }

    /// Applies mutations and advances to the next block.
    pub fn apply_mutations(
        &mut self,
        mutations: AccountMutationSet,
    ) -> Result<(), HistoricalError> {
        let rev = self.latest.apply_mutations_with_reversion(mutations)?;

        let block_num = self.block_number;
        let overlay = HistoricalOverlay::new(block_num, rev);
        self.overlays.insert(block_num, overlay);
        self.block_number = block_num.child();

        Self::drain_excess(&mut self.overlays);

        Ok(())
    }
}
