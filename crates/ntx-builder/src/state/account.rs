use std::collections::VecDeque;

use miden_objects::account::{Account, AccountDelta};

/// An account's current state including all inflight deltas.
///
/// Delta's can be added to the state and committed or reverted one at a time.
pub struct AccountState {
    /// Represents the foundation of the account state.
    ///
    /// This is either the most recently committed state, or the first uncommitted state of the
    /// account. The latter occurs if the transaction which created the account has not yet been
    /// committed.
    ///
    /// The `base_is_committed` flag distinguishes between these states. Ideally we would use an
    /// enum to represent this, however it is quite unwieldy in practice.
    ///
    /// We need to distinguish between these states so we can identify whether to commit `base` or
    /// the first delta.
    base: Account,

    /// `true` if `base` is committed.
    base_is_committed: bool,

    /// Inflight account delta's which are _not_ committed to the base state.
    deltas: VecDeque<AccountDelta>,
}

impl AccountState {
    /// Creates a new [`AccountState`] where the account state has not yet been committed.
    ///
    /// i.e. where the transaction creating the account is still inflight.
    pub fn new_inflight(account: Account) -> Self {
        Self {
            base: account,
            base_is_committed: false,
            deltas: VecDeque::default(),
        }
    }

    /// Creates a new [`AccountState`] where the state has previously been committed.
    ///
    /// e.g. the account state from an existing block.
    pub fn new_committed(account: Account) -> Self {
        Self {
            base: account,
            base_is_committed: true,
            deltas: VecDeque::default(),
        }
    }

    /// Adds a new delta to the stack.
    pub fn add_inflight_delta(&mut self, delta: AccountDelta) {
        self.deltas.push_back(delta);
    }

    /// Marks the oldest delta as committed, merging it into the committed account state.
    ///
    /// Committed account state _cannot_ be reverted.
    ///
    /// # Panics
    ///
    /// Panics if there are no inflight deltas to commit.
    pub fn commit_one(&mut self) {
        if !self.base_is_committed {
            self.base_is_committed = true;
            return;
        }

        let to_commit =
            self.deltas.pop_front().expect("account state should have a delta to commit");
        self.base.apply_delta(&to_commit).expect("account delta should apply");
    }

    /// Reverts the newest delta. If there are no delta's then the account creation
    /// itself will be reverted and [`None`] is returned in this case.
    ///
    /// # Panics
    ///
    /// Panics if this attempts to revert a previously committed account state.
    pub fn revert_one(mut self) -> Option<Self> {
        if self.deltas.pop_back().is_some() {
            return Some(self);
        }

        assert!(!self.base_is_committed, "attempted to revert a committed account state");
        None
    }

    /// A copy of the currently inflight account state.
    ///
    /// This _includes_ all uncommitted deltas.
    ///
    /// # Panics
    ///
    /// Panics if the deltas could not be applied.
    pub fn current_state(&self) -> Account {
        let mut account = self.base.clone();

        for delta in &self.deltas {
            account.apply_delta(delta).expect("account delta should apply");
        }

        account
    }
}
