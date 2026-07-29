use std::sync::Arc;

use miden_protocol::account::Account;
use miden_protocol::block::BlockHeader;
use miden_protocol::transaction::PartialBlockchain;
use miden_standards::note::AccountTargetNetworkNote;

// TRANSACTION CANDIDATE
// ================================================================================================

/// A candidate network transaction.
///
/// Contains the data pertaining to a specific network account which can be used to build a network
/// transaction.
#[derive(Clone, Debug)]
pub struct TransactionCandidate {
    /// The current inflight state of the account.
    ///
    /// Wrapped in `Arc` so building a candidate shares the actor's resident account instead of
    /// deep-cloning it (which, for accounts with large storage maps, is expensive). The account is
    /// only ever read during execution; the actor advances its own copy via `Arc::make_mut` once
    /// the candidate has been consumed.
    pub account: Arc<Account>,

    /// A set of notes addressed to this network account.
    pub notes: Vec<AccountTargetNetworkNote>,

    /// The latest locally committed block header.
    ///
    /// This should be used as the reference block during transaction execution.
    pub chain_tip_header: BlockHeader,

    /// The chain MMR, which lags behind the tip by one block.
    ///
    /// Wrapped in `Arc` to avoid expensive clones when reading the chain state.
    pub chain_mmr: Arc<PartialBlockchain>,
}
