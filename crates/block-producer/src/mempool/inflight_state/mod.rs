use std::collections::{BTreeMap, BTreeSet, VecDeque};

use miden_objects::account::AccountId;
use miden_objects::block::BlockNumber;
use miden_objects::note::{NoteId, Nullifier};
use miden_objects::transaction::TransactionId;

use crate::domain::transaction::AuthenticatedTransaction;
use crate::errors::{AddTransactionError, VerifyTxError};

mod account_state;

use account_state::InflightAccountState;

// IN-FLIGHT STATE
// ================================================================================================

/// Tracks the inflight state of the mempool. This includes recently committed blocks.
///
/// Allows appending and reverting transactions as well as marking them as part of a committed
/// block. Committed state can also be pruned once the state is considered past the stale
/// threshold.
#[derive(Clone, Debug, PartialEq)]
pub struct InflightState {
    /// Account states from inflight transactions.
    ///
    /// Accounts which are empty are immediately pruned.
    accounts: BTreeMap<AccountId, InflightAccountState>,

    /// Nullifiers produced by the input notes of inflight transactions.
    nullifiers: BTreeSet<Nullifier>,

    /// Notes created by inflight transactions.
    ///
    /// Some of these may already be consumed - check the nullifiers.
    output_notes: BTreeMap<NoteId, OutputNoteState>,

    /// Inflight transaction deltas.
    ///
    /// This _excludes_ deltas in committed blocks.
    transaction_deltas: BTreeMap<TransactionId, Delta>,

    /// Committed block deltas.
    committed_blocks: VecDeque<BTreeMap<TransactionId, Delta>>,

    /// Amount of recently committed blocks we retain in addition to the inflight state.
    ///
    /// This provides an overlap between committed and inflight state, giving a grace
    /// period for incoming transactions to be verified against both without requiring it
    /// to be an atomic action.
    num_retained_blocks: usize,

    /// The latest committed block height.
    chain_tip: BlockNumber,

    /// Number of blocks to allow between chain tip and a transaction's expiration block height
    /// before rejecting it.
    ///
    /// A new transaction is rejected if its expiration block is this close to the chain tip.
    expiration_slack: u32,
}

/// A summary of a transaction's impact on the state.
#[derive(Clone, Debug, PartialEq)]
struct Delta {
    /// The account this transaction updated.
    account: AccountId,
    /// The nullifiers produced by this transaction.
    nullifiers: BTreeSet<Nullifier>,
    /// The output notes created by this transaction.
    output_notes: BTreeSet<NoteId>,
}

impl Delta {
    fn new(tx: &AuthenticatedTransaction) -> Self {
        Self {
            account: tx.account_id(),
            nullifiers: tx.nullifiers().collect(),
            output_notes: tx.output_note_ids().collect(),
        }
    }
}

impl InflightState {
    /// Creates an [`InflightState`] which will retain committed state for the given
    /// amount of blocks before pruning them.
    pub fn new(chain_tip: BlockNumber, num_retained_blocks: usize, expiration_slack: u32) -> Self {
        Self {
            num_retained_blocks,
            chain_tip,
            expiration_slack,
            accounts: BTreeMap::default(),
            nullifiers: BTreeSet::default(),
            output_notes: BTreeMap::default(),
            transaction_deltas: BTreeMap::default(),
            committed_blocks: VecDeque::default(),
        }
    }

    /// Appends the transaction to the inflight state.
    ///
    /// This operation is atomic i.e. a rejected transaction has no impact of the state.
    pub fn add_transaction(
        &mut self,
        tx: &AuthenticatedTransaction,
    ) -> Result<BTreeSet<TransactionId>, AddTransactionError> {
        // Separate verification and state mutation so that a rejected transaction
        // does not impact the state (atomicity).
        self.verify_transaction(tx)?;

        let parents = self.insert_transaction(tx);

        Ok(parents)
    }

    fn oldest_committed_state(&self) -> BlockNumber {
        let committed_len: u32 = self
            .committed_blocks
            .len()
            .try_into()
            .expect("We should not be storing many blocks");
        self.chain_tip
            .checked_sub(committed_len)
            .expect("Chain height cannot be less than number of committed blocks")
    }

    fn verify_transaction(&self, tx: &AuthenticatedTransaction) -> Result<(), AddTransactionError> {
        // Check that the transaction hasn't already expired.
        if tx.expires_at() <= self.chain_tip + self.expiration_slack {
            return Err(AddTransactionError::Expired {
                expired_at: tx.expires_at(),
                limit: self.chain_tip + self.expiration_slack,
            });
        }

        // The mempool retains recently committed blocks, in addition to the state that is currently
        // inflight. This overlap with the committed state allows us to verify incoming
        // transactions against the current state (committed + inflight). Transactions are
        // first authenticated against the committed state prior to being submitted to the
        // mempool. The overlap provides a grace period between transaction authentication
        // against committed state and verification against inflight state.
        //
        // Here we just ensure that this authentication point is still within this overlap zone.
        // This should only fail if the grace period is too restrictive for the current
        // combination of block rate, transaction throughput and database IO.
        let stale_limit = self.oldest_committed_state();
        if tx.authentication_height() < stale_limit {
            return Err(AddTransactionError::StaleInputs {
                input_block: tx.authentication_height(),
                stale_limit,
            });
        }

        // Ensure current account state is correct.
        let current = self
            .accounts
            .get(&tx.account_id())
            .and_then(|account_state| account_state.current_state())
            .copied()
            .or(tx.store_account_state());
        let expected = tx.account_update().initial_state_commitment();

        // SAFETY: a new accounts state is set to zero ie default.
        if expected != current.unwrap_or_default() {
            return Err(VerifyTxError::IncorrectAccountInitialCommitment {
                tx_initial_account_commitment: expected,
                current_account_commitment: current,
            }
            .into());
        }

        // Ensure nullifiers aren't already present.
        //
        // We don't need to check the store state here because that was
        // already performed as part of authenticated the transaction.
        let double_spend = tx
            .nullifiers()
            .filter(|nullifier| self.nullifiers.contains(nullifier))
            .collect::<Vec<_>>();
        if !double_spend.is_empty() {
            return Err(VerifyTxError::InputNotesAlreadyConsumed(double_spend).into());
        }

        // Ensure output notes aren't already present.
        let duplicates = tx
            .output_note_ids()
            .filter(|note| self.output_notes.contains_key(note))
            .collect::<Vec<_>>();
        if !duplicates.is_empty() {
            return Err(VerifyTxError::OutputNotesAlreadyExist(duplicates).into());
        }

        // Ensure that all unauthenticated notes have an inflight output note to consume.
        //
        // We don't need to worry about double spending them since we already checked for
        // that using the nullifiers.
        //
        // Note that the authenticated transaction already filters out notes that were
        // previously unauthenticated, but were authenticated by the store.
        let missing = tx
            .unauthenticated_notes()
            .filter(|note_id| !self.output_notes.contains_key(note_id))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(VerifyTxError::UnauthenticatedNotesNotFound(missing).into());
        }

        Ok(())
    }

    /// Aggregate the transaction into the state, returning its parent transactions.
    fn insert_transaction(&mut self, tx: &AuthenticatedTransaction) -> BTreeSet<TransactionId> {
        self.transaction_deltas.insert(tx.id(), Delta::new(tx));
        let account_parent = self
            .accounts
            .entry(tx.account_id())
            .or_default()
            .insert(tx.account_update().final_state_commitment(), tx.id());

        self.nullifiers.extend(tx.nullifiers());
        self.output_notes
            .extend(tx.output_note_ids().map(|note_id| (note_id, OutputNoteState::new(tx.id()))));

        // Authenticated input notes (provably) consume notes that are already committed
        // on chain. They therefore cannot form part of the inflight dependency chain.
        //
        // Additionally, we only care about parents which have not been committed yet.
        let note_parents = tx
            .unauthenticated_notes()
            .filter_map(|note_id| self.output_notes.get(&note_id))
            .filter_map(|note| note.transaction())
            .copied();

        account_parent.into_iter().chain(note_parents).collect()
    }

    /// Reverts the given set of _uncommitted_ transactions.
    ///
    /// # Panics
    ///
    /// Panics if any transactions is not part of the uncommitted state. Callers should take care to
    /// only revert transaction sets who's ancestors are all either committed or reverted.
    pub fn revert_transactions(&mut self, txs: BTreeSet<TransactionId>) {
        for tx in txs {
            let delta = self.transaction_deltas.remove(&tx).expect("Transaction delta must exist");

            // SAFETY: Since the delta exists, so must the account.
            let account_status = self.accounts.get_mut(&delta.account).unwrap().revert(1);
            // Prune empty accounts.
            if account_status.is_empty() {
                self.accounts.remove(&delta.account);
            }

            for nullifier in delta.nullifiers {
                assert!(self.nullifiers.remove(&nullifier), "Nullifier must exist");
            }

            for note in delta.output_notes {
                assert!(self.output_notes.remove(&note).is_some(), "Output note must exist");
            }
        }
    }

    /// Marks the given state diff as committed.
    ///
    /// These transactions are no longer considered inflight. Callers should take care to only
    /// commit transactions who's ancestors are all committed.
    ///
    /// Note that this state is still retained for the configured number of blocks. The oldest
    /// retained block is also pruned from the state.
    ///
    /// # Panics
    ///
    /// Panics if any transactions is not part of the uncommitted state.
    pub fn commit_block(&mut self, txs: impl IntoIterator<Item = TransactionId>) {
        let mut block_deltas = BTreeMap::new();
        for tx in txs {
            let delta = self.transaction_deltas.remove(&tx).expect("Transaction delta must exist");

            // SAFETY: Since the delta exists, so must the account.
            self.accounts.get_mut(&delta.account).unwrap().commit(1);

            for note in &delta.output_notes {
                self.output_notes.get_mut(note).expect("Output note must exist").commit();
            }

            block_deltas.insert(tx, delta);
        }

        self.committed_blocks.push_back(block_deltas);
        self.prune_block();
        self.chain_tip = self.chain_tip.child();
    }

    /// Prunes the state from the oldest committed block _IFF_ there are more than the number we
    /// wish to retain.
    ///
    /// This is used to bound the size of the inflight state.
    fn prune_block(&mut self) {
        // Keep the required number of committed blocks.
        //
        // This would occur on startup until we have accumulated enough blocks.
        if self.committed_blocks.len() <= self.num_retained_blocks {
            return;
        }
        // SAFETY: The length check above guarantees that we have at least one committed block.
        let block = self.committed_blocks.pop_front().unwrap();

        for (_, delta) in block {
            // SAFETY: Since the delta exists, so must the account.
            let status = self.accounts.get_mut(&delta.account).unwrap().prune_committed(1);

            // Prune empty accounts.
            if status.is_empty() {
                self.accounts.remove(&delta.account);
            }

            for nullifier in delta.nullifiers {
                self.nullifiers.remove(&nullifier);
            }

            for output_note in delta.output_notes {
                self.output_notes.remove(&output_note);
            }
        }
    }

    /// The number of accounts affected by inflight transactions _and_ transactions in recently
    /// committed blocks.
    pub fn num_accounts(&self) -> usize {
        self.accounts.len()
    }

    /// The number of nullifiers produced by inflight transactions _and_ transactions in recently
    /// committed blocks.
    pub fn num_nullifiers(&self) -> usize {
        self.nullifiers.len()
    }

    /// The number of notes produced by inflight transactions _and_ transactions in recently
    /// committed blocks.
    pub fn num_notes_created(&self) -> usize {
        self.output_notes.len()
    }
}

/// Describes the state of an inflight output note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputNoteState {
    /// Output note is part of a committed block, and its source transaction should no longer be
    /// considered for dependency tracking.
    Committed,
    /// Output note is still inflight and should be considered for dependency tracking.
    Inflight(TransactionId),
}

impl OutputNoteState {
    /// Creates a new inflight output note state.
    fn new(tx: TransactionId) -> Self {
        Self::Inflight(tx)
    }

    /// Commits the output note, removing the source transaction.
    fn commit(&mut self) {
        *self = Self::Committed;
    }

    /// Returns the source transaction ID if the output note is not yet committed.
    fn transaction(&self) -> Option<&TransactionId> {
        if let Self::Inflight(tx) = self { Some(tx) } else { None }
    }
}

// TESTS
// ================================================================================================

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use miden_node_utils::ErrorReport;
    use miden_objects::Word;

    use super::*;
    use crate::test_utils::note::{mock_note, mock_output_note};
    use crate::test_utils::{MockProvenTxBuilder, mock_account_id};

    #[test]
    fn rejects_expired_transaction() {
        let chain_tip = BlockNumber::from(123);
        let mut uut = InflightState::new(chain_tip, 5, 0u32);

        let expired = MockProvenTxBuilder::with_account_index(0)
            .expiration_block_num(chain_tip)
            .build();
        let expired =
            AuthenticatedTransaction::from_inner(expired).with_authentication_height(chain_tip);

        let err = uut.add_transaction(&expired).unwrap_err();
        assert_matches!(err, AddTransactionError::Expired { .. });
    }

    /// Ensures that the specified expiration slack is adhered to.
    #[test]
    fn expiration_slack_is_respected() {
        let slack = 3;
        let chain_tip = BlockNumber::from(123);
        let expiration_limit = chain_tip + slack;
        let mut uut = InflightState::new(chain_tip, 5, slack);

        let unexpired = MockProvenTxBuilder::with_account_index(0)
            .expiration_block_num(expiration_limit + 1)
            .build();
        let unexpired =
            AuthenticatedTransaction::from_inner(unexpired).with_authentication_height(chain_tip);

        uut.add_transaction(&unexpired).unwrap();

        let expired = MockProvenTxBuilder::with_account_index(1)
            .expiration_block_num(expiration_limit)
            .build();
        let expired =
            AuthenticatedTransaction::from_inner(expired).with_authentication_height(chain_tip);

        let err = uut.add_transaction(&expired).unwrap_err();
        assert_matches!(err, AddTransactionError::Expired { .. });
    }

    #[test]
    fn rejects_duplicate_nullifiers() {
        let account = mock_account_id(1);
        let states = (1u8..=4).map(|x| Word::from([x, 0, 0, 0])).collect::<Vec<_>>();

        let note_seed = 123;
        // We need to make the note available first, in order for it to be consumed at all.
        let tx0 = MockProvenTxBuilder::with_account(account, states[0], states[1])
            .output_notes(vec![mock_output_note(note_seed)])
            .build();
        let tx1 = MockProvenTxBuilder::with_account(account, states[1], states[2])
            .unauthenticated_notes(vec![mock_note(note_seed)])
            .build();
        let tx2 = MockProvenTxBuilder::with_account(account, states[2], states[3])
            .unauthenticated_notes(vec![mock_note(note_seed)])
            .build();

        let mut uut = InflightState::new(BlockNumber::default(), 1, 0u32);
        uut.add_transaction(&AuthenticatedTransaction::from_inner(tx0)).unwrap();
        uut.add_transaction(&AuthenticatedTransaction::from_inner(tx1)).unwrap();

        let err = uut.add_transaction(&AuthenticatedTransaction::from_inner(tx2)).unwrap_err();

        assert_matches!(
            err,
            AddTransactionError::VerificationFailed(VerifyTxError::InputNotesAlreadyConsumed(
                notes
            )) if notes == vec![mock_note(note_seed).nullifier()]
        );
    }

    #[test]
    fn rejects_duplicate_output_notes() {
        let account = mock_account_id(1);
        let states = (1u8..=3).map(|x| Word::from([x, 0, 0, 0])).collect::<Vec<_>>();

        let note = mock_output_note(123);
        let tx0 = MockProvenTxBuilder::with_account(account, states[0], states[1])
            .output_notes(vec![note.clone()])
            .build();
        let tx1 = MockProvenTxBuilder::with_account(account, states[1], states[2])
            .output_notes(vec![note.clone()])
            .build();

        let mut uut = InflightState::new(BlockNumber::default(), 1, 0u32);
        uut.add_transaction(&AuthenticatedTransaction::from_inner(tx0)).unwrap();

        let err = uut.add_transaction(&AuthenticatedTransaction::from_inner(tx1)).unwrap_err();

        assert_matches!(
            err,
            AddTransactionError::VerificationFailed(VerifyTxError::OutputNotesAlreadyExist(
                notes
            )) if notes == vec![note.id()]
        );
    }

    #[test]
    fn rejects_account_state_mismatch() {
        let account = mock_account_id(1);
        let states = (1u8..=3).map(|x| Word::from([x, 0, 0, 0])).collect::<Vec<_>>();

        let tx = MockProvenTxBuilder::with_account(account, states[0], states[1]).build();

        let mut uut = InflightState::new(BlockNumber::default(), 1, 0u32);
        let err = uut
            .add_transaction(&AuthenticatedTransaction::from_inner(tx).with_store_state(states[2]))
            .unwrap_err();

        assert_matches!(
            err,
            AddTransactionError::VerificationFailed(VerifyTxError::IncorrectAccountInitialCommitment {
                tx_initial_account_commitment: init_state,
                current_account_commitment: current_state,
            }) if init_state == states[0] && current_state == states[2].into()
        );
    }

    #[test]
    fn account_state_transitions() {
        let account = mock_account_id(1);
        let states = (1u8..=3).map(|x| Word::from([x, 0, 0, 0])).collect::<Vec<_>>();

        let tx0 = MockProvenTxBuilder::with_account(account, states[0], states[1]).build();
        let tx1 = MockProvenTxBuilder::with_account(account, states[1], states[2]).build();

        let mut uut = InflightState::new(BlockNumber::default(), 1, 0u32);
        uut.add_transaction(&AuthenticatedTransaction::from_inner(tx0)).unwrap();
        uut.add_transaction(&AuthenticatedTransaction::from_inner(tx1).with_empty_store_state())
            .unwrap();
    }

    #[test]
    fn new_account_state_defaults_to_zero() {
        let account = mock_account_id(1);

        let tx = MockProvenTxBuilder::with_account(
            account,
            [0u8, 0, 0, 0].into(),
            [1u8, 0, 0, 0].into(),
        )
        .build();

        let mut uut = InflightState::new(BlockNumber::default(), 1, 0u32);
        uut.add_transaction(&AuthenticatedTransaction::from_inner(tx).with_empty_store_state())
            .unwrap();
    }

    #[test]
    fn inflight_account_state_overrides_input_state() {
        let account = mock_account_id(1);
        let states = (1u8..=3).map(|x| Word::from([x, 0, 0, 0])).collect::<Vec<_>>();

        let tx0 = MockProvenTxBuilder::with_account(account, states[0], states[1]).build();
        let tx1 = MockProvenTxBuilder::with_account(account, states[1], states[2]).build();

        let mut uut = InflightState::new(BlockNumber::default(), 1, 0u32);
        uut.add_transaction(&AuthenticatedTransaction::from_inner(tx0)).unwrap();

        // Feed in an old state via input. This should be ignored, and the previous tx's final
        // state should be used.
        uut.add_transaction(&AuthenticatedTransaction::from_inner(tx1).with_store_state(states[0]))
            .unwrap();
    }

    #[test]
    fn dependency_tracking() {
        let account = mock_account_id(1);
        let states = (1u8..=3).map(|x| Word::from([x, 0, 0, 0])).collect::<Vec<_>>();
        let note_seed = 123;

        // Parent via account state.
        let tx0 = MockProvenTxBuilder::with_account(account, states[0], states[1]).build();
        // Parent via output note.
        let tx1 = MockProvenTxBuilder::with_account(mock_account_id(2), states[0], states[1])
            .output_notes(vec![mock_output_note(note_seed)])
            .build();

        let tx = MockProvenTxBuilder::with_account(account, states[1], states[2])
            .unauthenticated_notes(vec![mock_note(note_seed)])
            .build();

        let mut uut = InflightState::new(BlockNumber::default(), 1, 0u32);
        uut.add_transaction(&AuthenticatedTransaction::from_inner(tx0.clone())).unwrap();
        uut.add_transaction(&AuthenticatedTransaction::from_inner(tx1.clone())).unwrap();

        let parents = uut
            .add_transaction(&AuthenticatedTransaction::from_inner(tx).with_empty_store_state())
            .unwrap();
        let expected = BTreeSet::from([tx0.id(), tx1.id()]);

        assert_eq!(parents, expected);
    }

    #[test]
    fn committed_parents_are_not_tracked() {
        let account = mock_account_id(1);
        let states = (1u8..=3).map(|x| Word::from([x, 0, 0, 0])).collect::<Vec<_>>();
        let note_seed = 123;

        // Parent via account state.
        let tx0 = MockProvenTxBuilder::with_account(account, states[0], states[1]).build();
        let tx0 = AuthenticatedTransaction::from_inner(tx0);
        // Parent via output note.
        let tx1 = MockProvenTxBuilder::with_account(mock_account_id(2), states[0], states[1])
            .output_notes(vec![mock_output_note(note_seed)])
            .build();
        let tx1 = AuthenticatedTransaction::from_inner(tx1);

        let tx = MockProvenTxBuilder::with_account(account, states[1], states[2])
            .unauthenticated_notes(vec![mock_note(note_seed)])
            .build();

        let mut uut = InflightState::new(BlockNumber::default(), 1, 0u32);
        uut.add_transaction(&tx0.clone()).unwrap();
        uut.add_transaction(&tx1.clone()).unwrap();

        // Commit the parents, which should remove them from dependency tracking.
        uut.commit_block([tx0.id(), tx1.id()]);

        let parents = uut
            .add_transaction(&AuthenticatedTransaction::from_inner(tx).with_empty_store_state())
            .unwrap();

        assert!(parents.is_empty());
    }

    #[test]
    fn tx_insertions_and_reversions_cancel_out() {
        // Reverting txs should be equivalent to them never being inserted.
        //
        // We test this by reverting some txs and equating it to the remaining set.
        // This is a form of property test.
        let states = (1u8..=5).map(|x| Word::from([x, 0, 0, 0])).collect::<Vec<_>>();
        let txs = vec![
            MockProvenTxBuilder::with_account(mock_account_id(1), states[0], states[1]),
            MockProvenTxBuilder::with_account(mock_account_id(1), states[1], states[2])
                .output_notes(vec![mock_output_note(111), mock_output_note(222)]),
            MockProvenTxBuilder::with_account(mock_account_id(2), states[0], states[1])
                .unauthenticated_notes(vec![mock_note(222)]),
            MockProvenTxBuilder::with_account(mock_account_id(1), states[2], states[3]),
            MockProvenTxBuilder::with_account(mock_account_id(2), states[1], states[2])
                .unauthenticated_notes(vec![mock_note(111)])
                .output_notes(vec![mock_output_note(45)]),
        ];

        let txs = txs
            .into_iter()
            .map(MockProvenTxBuilder::build)
            .map(AuthenticatedTransaction::from_inner)
            .collect::<Vec<_>>();

        for i in 0..states.len() {
            // Insert all txs and then revert the last `i` of them.
            // This should match only inserting the first `N-i` of them.
            let mut reverted = InflightState::new(BlockNumber::default(), 1, 0u32);
            for (idx, tx) in txs.iter().enumerate() {
                reverted.add_transaction(tx).unwrap_or_else(|err| {
                    panic!(
                        "Inserting tx #{idx} in iteration {i} should succeed: {}",
                        err.as_report()
                    )
                });
            }
            reverted.revert_transactions(
                txs.iter().rev().take(i).rev().map(AuthenticatedTransaction::id).collect(),
            );

            let mut inserted = InflightState::new(BlockNumber::default(), 1, 0u32);
            for (idx, tx) in txs.iter().rev().skip(i).rev().enumerate() {
                inserted.add_transaction(tx).unwrap_or_else(|err| {
                    panic!(
                        "Inserting tx #{idx} in iteration {i} should succeed: {}",
                        err.as_report()
                    )
                });
            }

            assert_eq!(reverted, inserted, "Iteration {i}");
        }
    }

    #[test]
    fn pruning_committed_state() {
        //! This is a form of property test, where we assert that pruning the first `i` of `N`
        //! transactions is equivalent to only inserting the last `N-i` transactions.
        let states = (1u8..=5).map(|x| Word::from([x, 0, 0, 0])).collect::<Vec<_>>();

        // Skipping initial txs means that output notes required for subsequent unauthenticated
        // input notes wont' always be present. To work around this, we instead only use
        // authenticated input notes.
        let txs = vec![
            MockProvenTxBuilder::with_account(mock_account_id(1), states[0], states[1]),
            MockProvenTxBuilder::with_account(mock_account_id(1), states[1], states[2])
                .output_notes(vec![mock_output_note(111), mock_output_note(222)]),
            MockProvenTxBuilder::with_account(mock_account_id(2), states[0], states[1])
                .nullifiers(vec![mock_note(222).nullifier()]),
            MockProvenTxBuilder::with_account(mock_account_id(1), states[2], states[3]),
            MockProvenTxBuilder::with_account(mock_account_id(2), states[1], states[2])
                .nullifiers(vec![mock_note(111).nullifier()])
                .output_notes(vec![mock_output_note(45)]),
        ];

        let txs = txs
            .into_iter()
            .map(MockProvenTxBuilder::build)
            .map(AuthenticatedTransaction::from_inner)
            .collect::<Vec<_>>();

        for i in 0..states.len() {
            // Insert all txs and then commit and prune the first `i` of them.
            //
            // This should match only inserting the final `N-i` transactions.
            // Note: we force all committed state to immediately be pruned by setting
            // it to zero.
            let mut committed = InflightState::new(BlockNumber::default(), 0, 0u32);
            for (idx, tx) in txs.iter().enumerate() {
                committed.add_transaction(tx).unwrap_or_else(|err| {
                    panic!(
                        "Inserting tx #{idx} in iteration {i} should succeed: {}",
                        err.as_report()
                    )
                });
            }
            committed.commit_block(txs.iter().take(i).map(AuthenticatedTransaction::id));

            let mut inserted = InflightState::new(BlockNumber::from(1), 0, 0u32);
            for (idx, tx) in txs.iter().skip(i).enumerate() {
                // We need to adjust the height since we are effectively at block "1" now.
                let tx = tx.clone().with_authentication_height(1.into());
                inserted.add_transaction(&tx).unwrap_or_else(|err| {
                    panic!(
                        "Inserting tx #{idx} in iteration {i} should succeed: {}",
                        err.as_report()
                    )
                });
            }

            assert_eq!(committed, inserted, "Iteration {i}");
        }
    }
}
