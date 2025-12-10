use std::collections::BTreeMap;

use miden_objects::Word;
use miden_objects::account::AccountId;
use miden_objects::block::BlockNumber;
use miden_objects::crypto::merkle::SmtForest;

/// Container for forest-related state that needs to be updated atomically.
pub(crate) struct InnerForest {
    /// `SmtForest` for efficient account storage reconstruction.
    /// Populated during block import with storage and vault SMTs.
    pub(crate) storage_forest: SmtForest,

    /// Maps (`account_id`, `slot_index`, `block_num`) to SMT root.
    /// Populated during block import for all storage map slots.
    pub(crate) storage_roots: BTreeMap<(AccountId, u8, BlockNumber), Word>,

    /// Maps (`account_id`, `block_num`) to vault SMT root.
    /// Tracks asset vault versions across all blocks with structural sharing.
    pub(crate) vault_roots: BTreeMap<(AccountId, BlockNumber), Word>,
}

impl InnerForest {
    pub(crate) fn new() -> Self {
        Self {
            storage_forest: SmtForest::new(),
            storage_roots: BTreeMap::new(),
            vault_roots: BTreeMap::new(),
        }
    }
}
