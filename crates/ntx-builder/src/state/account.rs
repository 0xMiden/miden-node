use std::collections::{HashMap, VecDeque};

use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_proto::domain::note::SingleTargetNetworkNote;
use miden_objects::account::delta::AccountUpdateDetails;
use miden_objects::account::{Account, AccountDelta, AccountId};
use miden_objects::block::BlockNumber;
use miden_objects::note::{Note, Nullifier};

// INFLIGHT NETWORK NOTE
// ================================================================================================

/// An unconsumed network note that may have failed to execute.
///
/// The block number at which the network note was attempted are approximate and may not
/// reflect the exact block number for which the execution attempt failed. The actual block
/// will likely be soon after the number that is recorded here.
#[derive(Debug, Clone)]
pub struct InflightNetworkNote {
    note: SingleTargetNetworkNote,
    attempt_count: usize,
    last_attempt: Option<BlockNumber>,
}

impl InflightNetworkNote {
    /// Creates a new inflight network note.
    pub fn new(note: SingleTargetNetworkNote) -> Self {
        Self {
            note,
            attempt_count: 0,
            last_attempt: None,
        }
    }

    /// Consumes the inflight network note and returns the inner network note.
    pub fn into_inner(self) -> SingleTargetNetworkNote {
        self.note
    }

    /// Returns a reference to the inner network note.
    pub fn to_inner(&self) -> &SingleTargetNetworkNote {
        &self.note
    }

    /// Returns the number of attempts made to execute the network note.
    pub fn attempt_count(&self) -> usize {
        self.attempt_count
    }

    /// Checks if the network note is available for execution.
    ///
    /// The note is available if it can be consumed and the backoff period has passed.
    pub fn is_available(&self, block_num: BlockNumber) -> bool {
        let can_consume = self
            .to_inner()
            .metadata()
            .execution_hint()
            .can_be_consumed(block_num)
            .unwrap_or(true);
        can_consume && has_backoff_passed(block_num, self.last_attempt, self.attempt_count)
    }

    /// Registers a failed attempt to execute the network note at the specified block number.
    pub fn fail(&mut self, block_num: BlockNumber) {
        self.last_attempt = Some(block_num);
        self.attempt_count += 1;
    }
}

impl From<InflightNetworkNote> for Note {
    fn from(value: InflightNetworkNote) -> Self {
        value.into_inner().into()
    }
}

// ACCOUNT STATE
// ================================================================================================

/// Tracks the state of a network account and its notes.
pub struct AccountState {
    /// The committed account state, if any.
    ///
    /// Its possible this is `None` if the account creation transaction is still inflight.
    committed: Option<Account>,

    /// Inflight account updates in chronological order.
    inflight: VecDeque<Account>,

    /// Unconsumed notes of this account.
    available_notes: HashMap<Nullifier, InflightNetworkNote>,

    /// Notes which have been consumed by transactions that are still inflight.
    nullified_notes: HashMap<Nullifier, InflightNetworkNote>,
}

impl AccountState {
    /// Creates a new account state using the given value as the committed state.
    pub fn from_committed_account(account: Account) -> Self {
        Self {
            committed: Some(account),
            inflight: VecDeque::default(),
            available_notes: HashMap::default(),
            nullified_notes: HashMap::default(),
        }
    }

    /// Creates a new account state where the creating transaction is still inflight.
    pub fn from_uncommitted_account(account: Account) -> Self {
        Self {
            inflight: VecDeque::from([account]),
            committed: None,
            available_notes: HashMap::default(),
            nullified_notes: HashMap::default(),
        }
    }

    /// Returns an iterator over inflight notes that are not currently within their respective
    /// backoff periods based on block number.
    pub fn available_notes(
        &self,
        block_num: &BlockNumber,
    ) -> impl Iterator<Item = &InflightNetworkNote> {
        self.available_notes.values().filter(|&note| note.is_available(*block_num))
    }

    /// Appends a delta to the set of inflight account updates.
    pub fn add_delta(&mut self, delta: &AccountDelta) {
        let mut state = self.latest_account();
        state
            .apply_delta(delta)
            .expect("network account delta should apply since it was accepted by the mempool");

        self.inflight.push_back(state);
    }

    /// Commits the oldest account state delta.
    ///
    /// # Panics
    ///
    /// Panics if there are no deltas to commit.
    pub fn commit_delta(&mut self) {
        self.committed = self.inflight.pop_front().expect("must have a delta to commit").into();
    }

    /// Reverts the newest account state delta.
    ///
    /// # Returns
    ///
    /// Returns `true` if this reverted the account creation delta. The caller _must_ remove this
    /// account and associated notes as calls to `account` will panic.
    ///
    /// # Panics
    ///
    /// Panics if there are no deltas to revert.
    #[must_use = "must remove this account and its notes"]
    pub fn revert_delta(&mut self) -> bool {
        self.inflight.pop_back().expect("must have a delta to revert");
        self.committed.is_none() && self.inflight.is_empty()
    }

    /// Adds a new network note making it available for consumption.
    pub fn add_note(&mut self, note: SingleTargetNetworkNote) {
        self.available_notes.insert(note.nullifier(), InflightNetworkNote::new(note));
    }

    /// Removes the note completely.
    pub fn revert_note(&mut self, note: Nullifier) {
        // Transactions can be reverted out of order.
        //
        // This means the tx which nullified the note might not have been reverted yet, and the note
        // might still be in the nullified
        self.available_notes.remove(&note);
        self.nullified_notes.remove(&note);
    }

    /// Marks a note as being consumed.
    ///
    /// The note data is retained until the nullifier is committed.
    ///
    /// # Panics
    ///
    /// Panics if the note does not exist or was already nullified.
    pub fn add_nullifier(&mut self, nullifier: Nullifier) -> Result<(), ()> {
        if let Some(note) = self.available_notes.remove(&nullifier) {
            self.nullified_notes.insert(nullifier, note);
            Ok(())
        } else {
            tracing::warn!(%nullifier, "note must be available to nullify");
            Err(())
        }
    }

    /// Marks a nullifier as being committed, removing the associated note data entirely.
    ///
    /// # Panics
    ///
    /// Panics if the associated note is not marked as nullified.
    pub fn commit_nullifier(&mut self, nullifier: Nullifier) {
        // we might not have this if we didn't add it with `add_nullifier`
        // in case it's transaction wasn't available in the first place.
        // It shouldn't happen practically, since we skip them if the
        // relevant account cannot be retrieved via `fetch`.
        let _ = self.nullified_notes.remove(&nullifier);
    }

    /// Reverts a nullifier, marking the associated note as available again.
    pub fn revert_nullifier(&mut self, nullifier: Nullifier) {
        // Transactions can be reverted out of order.
        //
        // The note may already have been fully removed by `revert_note` if the transaction creating
        // the note was reverted before the transaction that consumed it.
        if let Some(note) = self.nullified_notes.remove(&nullifier) {
            self.available_notes.insert(nullifier, note);
        }
    }

    /// Drops all notes that have failed to be consumed after a certain number of attempts.
    pub fn drop_failing_notes(&mut self, max_attempts: usize) {
        self.available_notes.retain(|_, note| note.attempt_count() < max_attempts);
    }

    /// Returns the latest inflight account state.
    pub fn latest_account(&self) -> Account {
        self.inflight
            .back()
            .or(self.committed.as_ref())
            .expect("account must have either a committed or inflight state")
            .clone()
    }

    /// Returns `true` if there is no inflight state being tracked.
    ///
    /// This implies this state is safe to remove without losing uncommitted data.
    pub fn is_empty(&self) -> bool {
        self.inflight.is_empty()
            && self.available_notes.is_empty()
            && self.nullified_notes.is_empty()
    }

    /// Marks the specified notes as failed.
    pub fn fail_notes(&mut self, nullifiers: &[Nullifier], block_num: BlockNumber) {
        for nullifier in nullifiers {
            if let Some(note) = self.available_notes.get_mut(nullifier) {
                note.fail(block_num);
            } else {
                tracing::warn!(%nullifier, "failed note is not in account's state");
            }
        }
    }
}

// NETWORK ACCOUNT UPDATE
// ================================================================================================

#[derive(Clone)]
pub enum NetworkAccountUpdate {
    New(Account),
    Delta(AccountDelta),
}

impl NetworkAccountUpdate {
    pub fn from_protocol(update: AccountUpdateDetails) -> Option<Self> {
        let update = match update {
            AccountUpdateDetails::Private => return None,
            AccountUpdateDetails::New(update) => Self::New(update),
            AccountUpdateDetails::Delta(update) => Self::Delta(update),
        };

        update.account_id().is_network().then_some(update)
    }

    pub fn prefix(&self) -> NetworkAccountPrefix {
        // SAFETY: This is a network account by construction.
        self.account_id().try_into().unwrap()
    }

    fn account_id(&self) -> AccountId {
        match self {
            NetworkAccountUpdate::New(account) => account.id(),
            NetworkAccountUpdate::Delta(account_delta) => account_delta.id(),
        }
    }
}

// HELPERS
// ================================================================================================

/// Checks if the backoff block period has passed.
///
/// The number of blocks passed since the last attempt must be greater than or equal to
/// e^(0.25 * `attempt_count`) rounded to the nearest integer.
///
/// This evaluates to the following:
/// - After 1 attempt, the backoff period is 1 block.
/// - After 3 attempts, the backoff period is 2 blocks.
/// - After 10 attempts, the backoff period is 12 blocks.
/// - After 20 attempts, the backoff period is 148 blocks.
/// - etc...
#[allow(clippy::cast_precision_loss, clippy::cast_sign_loss)]
fn has_backoff_passed(
    chain_tip: BlockNumber,
    last_attempt: Option<BlockNumber>,
    attempts: usize,
) -> bool {
    if attempts == 0 {
        return true;
    }
    // Compute the number of blocks passed since the last attempt.
    let blocks_passed = last_attempt
        .and_then(|last| chain_tip.checked_sub(last.as_u32()))
        .unwrap_or_default();

    // Compute the exponential backoff threshold: Δ = e^(0.25 * n).
    let backoff_threshold = (0.25 * attempts as f64).exp().round() as usize;

    // Check if the backoff period has passed.
    blocks_passed.as_usize() > backoff_threshold
}

#[cfg(test)]
mod tests {
    use miden_objects::block::BlockNumber;

    #[rstest::rstest]
    #[test]
    #[case::all_zero(Some(BlockNumber::from(0)), BlockNumber::from(0), 0, true)]
    #[case::no_attempts(None, BlockNumber::from(0), 0, true)]
    #[case::one_attempt(Some(BlockNumber::from(0)), BlockNumber::from(2), 1, true)]
    #[case::three_attempts(Some(BlockNumber::from(0)), BlockNumber::from(3), 3, true)]
    #[case::ten_attempts(Some(BlockNumber::from(0)), BlockNumber::from(13), 10, true)]
    #[case::twenty_attempts(Some(BlockNumber::from(0)), BlockNumber::from(149), 20, true)]
    #[case::one_attempt_false(Some(BlockNumber::from(0)), BlockNumber::from(1), 1, false)]
    #[case::three_attempts_false(Some(BlockNumber::from(0)), BlockNumber::from(2), 3, false)]
    #[case::ten_attempts_false(Some(BlockNumber::from(0)), BlockNumber::from(12), 10, false)]
    #[case::twenty_attempts_false(Some(BlockNumber::from(0)), BlockNumber::from(148), 20, false)]
    fn backoff_has_passed(
        #[case] last_attempt_block_num: Option<BlockNumber>,
        #[case] current_block_num: BlockNumber,
        #[case] attempt_count: usize,
        #[case] backoff_should_have_passed: bool,
    ) {
        assert_eq!(
            backoff_should_have_passed,
            super::has_backoff_passed(current_block_num, last_attempt_block_num, attempt_count)
        );
    }
}
