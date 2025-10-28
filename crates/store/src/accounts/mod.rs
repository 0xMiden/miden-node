//! Historical tracking for `AccountTree` via mutation overlays

use std::collections::BTreeMap;

use miden_crypto::merkle::{EmptySubtreeRoots, MerklePath};
use miden_objects::{
    account::AccountId,
    block::{AccountMutationSet, AccountTree, AccountWitness, BlockNumber},
    crypto::merkle::{
        LargeSmt, LeafIndex, MemoryStorage, MerkleError, NodeIndex, NodeMutation, SmtLeaf,
        SmtStorage, SparseMerklePath, SMT_DEPTH,
    },
    AccountTreeError, EMPTY_WORD, Word,
};

/// Convenience for an in-memory-only account tree.
pub type InMemoryAccountTree = AccountTree<LargeSmt<MemoryStorage>>;

// ACCOUNT TREE STORAGE TRAIT
// ================================================================================================

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

// ERROR TYPES
// ================================================================================================

#[allow(missing_docs)]
#[derive(thiserror::Error, Debug)]
pub enum HistoricalError {
    #[error(transparent)]
    MerkleError(#[from] MerkleError),
    #[error(transparent)]
    AccountTreeError(#[from] AccountTreeError),
}

// HISTORICAL STATE ENUM
// ================================================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoricalState {
    /// The requested block is in the future (later than current block).
    Future,
    /// The requested block is available in history.
    Target(BlockNumber),
    /// The requested block is the current/latest block.
    Latest,
    /// The requested block is too old and has been pruned from history.
    TooAncient,
}

// HISTORICAL OVERLAY
// ================================================================================================

/// Captures reversion state for historical queries at a specific block.
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

// ACCOUNT TREE WITH HISTORY
// ================================================================================================

/// Wraps `AccountTree` with historical query support via reversion overlays.
///
/// This structure maintains a sliding window of historical account states by storing
/// reversion data (mutations that undo changes). Historical witnesses are reconstructed
/// by starting from the latest state and applying reversion overlays backwards in time.
#[derive(Debug, Clone)]
pub struct AccountTreeWithHistory<S>
where
    S: AccountTreeStorage,
{
    /// The current block number (latest state).
    block_number: BlockNumber,
    /// The latest account tree state.
    latest: S,
    /// Historical overlays indexed by block number, storing reversion data.
    overlays: BTreeMap<BlockNumber, HistoricalOverlay>,
}

impl<S> AccountTreeWithHistory<S>
where
    S: AccountTreeStorage,
{
    /// Maximum number of historical blocks to maintain.
    pub const MAX_HISTORY: usize = 33;

    // CONSTRUCTORS
    // --------------------------------------------------------------------------------------------

    /// Creates a new historical tree starting at the given block number.
    pub fn new(account_tree: S, block_number: BlockNumber) -> Self {
        Self {
            block_number,
            latest: account_tree,
            overlays: BTreeMap::new(),
        }
    }

    /// Removes oldest overlays when exceeding the maximum history depth.
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

    /// Returns the root hash of the latest state.
    pub fn root_latest(&self) -> Word {
        self.latest.root()
    }

    /// Returns the root hash at a specific historical block.
    ///
    /// Returns `None` if the block is in the future or too old (pruned).
    pub fn root_at(&self, block_number: BlockNumber) -> Option<Word> {
        match self.historical_state(block_number) {
            HistoricalState::Latest => Some(self.latest.root()),
            HistoricalState::Target(block_number) => {
                let overlay = self.overlays.get(&block_number)?;
                debug_assert_eq!(overlay.block_number, block_number);
                Some(overlay.rev_set.as_mutation_set().root())
            },
            HistoricalState::Future | HistoricalState::TooAncient => None,
        }
    }

    /// Returns the number of accounts in the latest state.
    pub fn num_accounts_latest(&self) -> usize {
        self.latest.num_accounts()
    }

    /// Returns the number of historical blocks currently stored.
    pub fn history_len(&self) -> usize {
        self.overlays.len()
    }

    /// Opens an account at the latest block, returning its witness.
    pub fn open_latest(&self, account_id: AccountId) -> AccountWitness {
        self.latest.open(account_id)
    }

    /// Opens an account at a historical block, returning its witness.
    ///
    /// This method reconstructs the account witness at the given historical block by:
    /// 1. Starting with the latest account state
    /// 2. Applying reversion mutations from the overlays to walk back in time
    /// 3. Reconstructing the Merkle path with the historical node values
    ///
    /// Returns `None` if the block is in the future or too old (pruned).
    pub fn open_at(
        &self,
        account_id: AccountId,
        block_number: BlockNumber,
    ) -> Option<AccountWitness> {
        match self.historical_state(block_number) {
            HistoricalState::Latest => Some(self.latest.open(account_id)),
            HistoricalState::Target(block_number) => {
                // Ensure overlay exists before reconstruction
                self.overlays.get(&block_number)?;
                Self::reconstruct_historical_witness(self, account_id, block_number)
            },
            HistoricalState::Future | HistoricalState::TooAncient => None,
        }
    }

    /// Gets the account state commitment at the latest block.
    pub fn get(&self, account_id: AccountId) -> Word {
        self.latest.get(account_id)
    }

    /// Checks if the tree contains an account with the given prefix.
    pub fn contains_account_id_prefix(
        &self,
        prefix: miden_objects::account::AccountIdPrefix,
    ) -> bool {
        self.latest.contains_account_id_prefix(prefix)
    }

    /// Determines the historical state of a requested block number.
    pub fn historical_state(&self, desired_block_number: BlockNumber) -> HistoricalState {
        if desired_block_number == self.block_number {
            return HistoricalState::Latest;
        }

        // Check if block is in the future
        if self.block_number.checked_sub(desired_block_number.as_u32()).is_none() {
            return HistoricalState::Future;
        }

        // Check if block exists in overlays
        if self.overlays.get(&desired_block_number).is_none() {
            return HistoricalState::TooAncient;
        }

        HistoricalState::Target(desired_block_number)
    }

    // PRIVATE HELPERS - HISTORICAL RECONSTRUCTION
    // --------------------------------------------------------------------------------------------

    /// Reconstructs a historical account witness by applying reversion overlays.
    fn reconstruct_historical_witness(
        &self,
        account_id: AccountId,
        block_target: BlockNumber,
    ) -> Option<AccountWitness> {
        // Start with the latest witness
        let latest_witness = self.latest.open(account_id);
        let (latest_path, leaf) = latest_witness.into_proof().into_parts();
        let (initial_mask, mut latest_nodes) = latest_path.into_parts();

        // Reverse nodes: SparseMerklePath stores them from root to leaf (high to low depth),
        // but we need leaf to root (low to high depth) for indexing by depth.
        latest_nodes.reverse();
        let path_nodes = Self::initialize_path_nodes(initial_mask, &latest_nodes);

        let leaf_index = NodeIndex::from(leaf.index());

        // Apply reversion overlays to reconstruct historical state
        let (path, leaf) = Self::apply_reversion_overlays(
            &self.overlays,
            block_target,
            path_nodes,
            leaf_index,
            leaf,
        )?;

        // Extract commitment from leaf
        let commitment = match leaf {
            SmtLeaf::Empty(_) => EMPTY_WORD,
            SmtLeaf::Single((_, value)) => value,
            SmtLeaf::Multiple(_) => unreachable!("AccountTree uses prefix-free IDs"),
        };

        AccountWitness::new(account_id, commitment, path).ok()
    }

    /// Initializes the path nodes array from the latest state.
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
            // Bit at position `depth` being 0 means node is present; 1 means empty
            let is_present = (initial_mask & (1u64 << depth)) == 0;
            if is_present && node_idx < latest_nodes.len() {
                *path_node = Some(latest_nodes[node_idx]);
                node_idx += 1;
            }
        }

        path_nodes
    }

    /// Applies reversion overlays to reconstruct the historical state.
    ///
    /// Iterates through overlays from newest to oldest (walking backwards in time),
    /// updating both the path nodes and the leaf value based on reversion mutations.
    fn apply_reversion_overlays(
        overlays: &BTreeMap<BlockNumber, HistoricalOverlay>,
        block_target: BlockNumber,
        mut path_nodes: [Option<Word>; SMT_DEPTH as usize],
        leaf_index: NodeIndex,
        mut leaf: SmtLeaf,
    ) -> Option<(SparseMerklePath, SmtLeaf)> {
        // Iterate through overlays in reverse (newest to oldest)
        for (_, overlay) in
            overlays.iter().rev().take_while(|(block_num, _)| block_target <= **block_num)
        {
            let rev_muts = overlay.rev_set.as_mutation_set().node_mutations();

            // Update path sibling nodes that changed in this overlay
            for sibling in leaf_index.proof_indices() {
                // Convert depth to height: depth 0 is root, we need height from leaf
                // depth() returns values from 1 (leaf) to 64 (root), so subtract 1 for 0-indexed
                let height = sibling
                    .depth()
                    .checked_sub(1) // -1: Convert from 1-indexed to 0-indexed
                    .expect("proof_indices should not include root") as usize;

                // Apply reversion mutation if this node was modified
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
            }

            // Update leaf if it was modified in this overlay
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

        // Build the Merkle path from reconstructed nodes
        let path = Self::build_dense_path(&path_nodes)?;
        let path = SparseMerklePath::try_from(path).ok()?;
        Some((path, leaf))
    }

    /// Builds a dense Merkle path from the path nodes array.
    ///
    /// Empty nodes are filled with their corresponding empty subtree roots.
    /// The path is built from root to leaf (high to low depth).
    fn build_dense_path(path_nodes: &[Option<Word>; SMT_DEPTH as usize]) -> Option<MerklePath> {
        let dense: Vec<Word> = (0..SMT_DEPTH)
            .rev() // Iterate from depth 63 down to 0 (root to leaf)
            .map(|d| {
                path_nodes[d as usize].as_ref().cloned().unwrap_or_else(|| {
                    // d+1: EmptySubtreeRoots expects depth from leaf (depth 0 at leaf)
                    EmptySubtreeRoots::entry(SMT_DEPTH, d + 1).clone()
                })
            })
            .collect();

        Some(MerklePath::new(dense))
    }

    // PUBLIC MUTATORS
    // --------------------------------------------------------------------------------------------

    /// Computes and applies mutations in one operation.
    ///
    /// This is a convenience method primarily for testing.
    pub fn compute_and_apply_mutations(
        &mut self,
        account_commitments: impl IntoIterator<Item = (AccountId, Word)>,
    ) -> Result<(), HistoricalError> {
        let mutations = self.compute_mutations(account_commitments)?;
        self.apply_mutations(mutations)
    }

    /// Computes mutations relative to the latest state.
    pub fn compute_mutations(
        &mut self,
        account_commitments: impl IntoIterator<Item = (AccountId, Word)>,
    ) -> Result<AccountMutationSet, HistoricalError> {
        Ok(self.latest.compute_mutations(account_commitments)?)
    }

    /// Applies mutations and advances to the next block.
    ///
    /// This method:
    /// 1. Applies the mutations to the latest tree, getting back reversion data
    /// 2. Stores the reversion data as a historical overlay
    /// 3. Advances the block number
    /// 4. Prunes old overlays if exceeding MAX_HISTORY
    pub fn apply_mutations(
        &mut self,
        mutations: AccountMutationSet,
    ) -> Result<(), HistoricalError> {
        // Apply mutations and get reversion data
        let rev = self.latest.apply_mutations_with_reversion(mutations)?;

        // Store reversion data for current block before advancing
        let block_num = self.block_number;
        let overlay = HistoricalOverlay::new(block_num, rev);
        self.overlays.insert(block_num, overlay);

        // Advance to next block
        self.block_number = block_num.child();

        // Prune old history if needed
        Self::drain_excess(&mut self.overlays);

        Ok(())
    }
}
