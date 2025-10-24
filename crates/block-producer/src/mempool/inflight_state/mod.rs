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
        self.verify_transaction(tx).inspect_err(|e| {
            tracing::warn!("Error on auth'd tx submission: {e:?}");
        })?;

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
mod tests;
