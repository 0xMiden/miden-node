//! Historical tracking for `AccountTree` via mutation overlays

use std::collections::VecDeque;
use std::sync::{Arc, RwLock};

use miden_crypto::EMPTY_WORD;
use miden_crypto::merkle::{
    LeafIndex,
    MerkleError,
    NodeIndex,
    NodeMutation,
    SMT_DEPTH,
    SmtLeaf,
    SparseMerklePath,
};
use miden_objects::account::AccountId;
use miden_objects::block::{AccountMutationSet, AccountTree, AccountWitness, BlockNumber};
use miden_objects::{AccountTreeError, Word};

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

/// captures reversion state for historical queries
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
struct InnerState {
    block_number: BlockNumber,
    latest: AccountTree,
    overlays: VecDeque<Arc<HistoricalOverlay>>,
}

impl InnerState {
    pub fn historical_offset(&self, desired_block_number: BlockNumber) -> HistoricalOffset {
        let Some(past_offset) = self.block_number.checked_sub(desired_block_number.as_u32()) else {
            return HistoricalOffset::Future;
        };
        let past_offset = past_offset.as_usize();
        match past_offset {
            0 => HistoricalOffset::Latest,
            1..=AccountTreeWithHistory::MAX_HISTORY => {
                HistoricalOffset::ReversionsIdx(past_offset - 1)
            },
            _ => HistoricalOffset::TooAncient,
        }
    }
}

/// wraps `AccountTree` with historical query support via reversion overlays
#[derive(Debug, Clone)]
pub struct AccountTreeWithHistory {
    inner: Arc<RwLock<InnerState>>,
}

impl AccountTreeWithHistory {
    pub const MAX_HISTORY: usize = 33;

    // CONSTRUCTORS
    // --------------------------------------------------------------------------------------------

    /// creates new historical tree
    pub fn new(account_tree: AccountTree, block_number: BlockNumber) -> Self {
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

    /// returns latest block number
    pub fn block_number(&self) -> BlockNumber {
        self.inner.read().unwrap().block_number
    }

    /// returns latest root
    pub fn root(&self) -> Word {
        self.inner.read().unwrap().latest.root()
    }

    /// returns root at given historical block
    pub fn root_at(&self, block_number: BlockNumber) -> Option<Word> {
        let guard = self.inner.read().unwrap();

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

    /// returns account count
    pub fn num_accounts(&self) -> usize {
        self.inner.read().unwrap().latest.num_accounts()
    }

    /// returns history depth
    pub fn history_len(&self) -> usize {
        self.inner.read().unwrap().overlays.len()
    }

    /// opens account at latest block
    pub fn open(&self, account_id: AccountId) -> AccountWitness {
        self.inner.read().unwrap().latest.open(account_id)
    }

    /// opens account at historical block
    pub fn open_at(
        &self,
        account_id: AccountId,
        block_number: BlockNumber,
    ) -> Option<AccountWitness> {
        let guard = self.inner.read().unwrap();

        match guard.historical_offset(block_number) {
            HistoricalOffset::Latest => Some(guard.latest.open(account_id)),
            HistoricalOffset::ReversionsIdx(idx) => {
                if idx >= guard.overlays.len() {
                    return None;
                }

                let latest_witness = guard.latest.open(account_id);
                let (latest_path, mut leaf) = latest_witness.into_proof().into_parts();
                let (initial_mask, mut latest_nodes) = latest_path.into_parts();

                // Reverse latest_nodes because they come in high-to-low depth order
                // but we need to initialize path_nodes in low-to-high depth order
                latest_nodes.reverse();

                // initialize the path nodes with the latest state, and update repeatedly.
                let mut path_nodes = [None; SMT_DEPTH as usize];
                let mut node_idx = 0;
                for (depth, path_node) in path_nodes.iter_mut().enumerate().take(SMT_DEPTH as usize)
                {
                    if (initial_mask & (1u64 << depth)) == 0 && node_idx < latest_nodes.len() {
                        *path_node = Some(latest_nodes[node_idx]);
                        node_idx += 1;
                    }
                }

                let leaf_index = NodeIndex::from(leaf.index());

                // We now go through all overlays, from newest to oldest to go back in "block-time".
                //
                // 1. for each overlay, apply update the relevant path sibling nodes that got
                //    changed, and lookup all the other nodes in the `latest` state, since they
                //    didn't change
                // 2. then reconstruct the `SparseMerklePath`
                for overlay in guard.overlays.iter().take(idx + 1) {
                    let rev_muts = overlay.rev_set.as_mutation_set().node_mutations();

                    for sibling in leaf_index.proof_indices() {
                        let height =
                            sibling.depth().checked_sub(1).expect("we don't transgress the root")
                                as usize;

                        // check reversion mutations first
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
                        // if an entry is not present in an overlay, we use the previous
                        // value from latest, which is already present.
                    }

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

                let max_depth =
                    path_nodes.iter().rposition(std::option::Option::is_some).map_or(0, |d| d + 1);
                let mut empty_mask = u64::MAX;
                let nodes: Vec<Word> = (0..max_depth)
                    .rev()
                    .filter_map(|d| {
                        path_nodes[d].inspect(|_| {
                            empty_mask &= !(1u64 << d);
                        })
                    })
                    .collect();

                let path = SparseMerklePath::from_parts(empty_mask, nodes).ok()?;
                let commitment = match leaf {
                    SmtLeaf::Empty(_) => EMPTY_WORD,
                    SmtLeaf::Single((_, value)) => value,
                    SmtLeaf::Multiple(_) => unreachable!("AccountTree uses prefix-free IDs"),
                };

                AccountWitness::new(account_id, commitment, path).ok()
            },
            HistoricalOffset::Future | HistoricalOffset::TooAncient => None,
        }
    }

    /// gets account state at latest block
    pub fn get(&self, account_id: AccountId) -> Word {
        self.inner.read().unwrap().latest.get(account_id)
    }

    // PUBLIC MUTATORS
    // --------------------------------------------------------------------------------------------

    /// applies mutations and advances to next block
    pub fn apply_mutations(
        &self,
        account_commitments: impl IntoIterator<Item = (AccountId, Word)>,
    ) -> Result<(), HistoricalError> {
        let mut inner = self.inner.write().unwrap();

        let accounts = Vec::from_iter(account_commitments);

        let mutations = inner.latest.compute_mutations(accounts)?;
        let rev = inner.latest.apply_mutations_with_reversion(mutations)?;
        let overlay = HistoricalOverlay::new(inner.block_number, rev);

        inner.overlays.push_front(Arc::new(overlay));
        inner.block_number = inner.block_number.child();
        Self::cleanup(&mut inner.overlays);

        Ok(())
    }

    /// gets account commitments in latest state
    pub fn account_commitments(&self) -> Vec<(AccountId, Word)> {
        self.inner.read().unwrap().latest.account_commitments().collect()
    }

    /// returns oldest block still in history
    pub fn oldest_block_num(&self) -> BlockNumber {
        let inner = self.inner.read().unwrap();
        inner
            .block_number
            .checked_sub(inner.overlays.len() as u32)
            .unwrap_or(BlockNumber::GENESIS)
    }

    /// checks if tree contains account prefix
    pub fn contains_account_id_prefix(
        &self,
        prefix: miden_objects::account::AccountIdPrefix,
    ) -> bool {
        self.inner.read().unwrap().latest.contains_account_id_prefix(prefix)
    }
}
