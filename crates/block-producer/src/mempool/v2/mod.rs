#![allow(unreachable_code, dead_code, unused_variables, reason = "wip")]

use std::collections::{HashMap, HashSet};

use miden_objects::Word;
use miden_objects::account::AccountId;
use miden_objects::batch::{BatchId, ProvenBatch};
use miden_objects::block::BlockNumber;
use miden_objects::note::{NoteId, Nullifier};
use miden_objects::transaction::TransactionId;

use crate::domain::transaction::AuthenticatedTransaction;
use crate::mempool::{BatchBudget, BlockBudget};

#[derive(Debug, thiserror::Error)]
enum SubmissionError {
    #[error("transaction {0} is already in the mempool")]
    DuplicateTransaction(TransactionId),
    #[error("transaction {0} builds on incomplete transactions")]
    IncompleteParents(TransactionId),
    #[error("transaction {0} is already in a batch")]
    AlreadyInBatch(TransactionId),
    #[error("nullifier {0} has already been spent")]
    DoubleSpend(Nullifier),
    #[error("note {0} already exists")]
    DuplicateNote(NoteId),
    #[error("unauthenticated note {0} not found")]
    UnknownNote(NoteId),
    #[error("account {account} has state {current} but data specified {expected}")]
    IncorrectAccountCommitment {
        account: AccountId,
        expected: Word,
        current: Word,
    },

    #[error("batch or transaction expires at {expired_at} which exceeds the limit of {limit}")]
    Expired {
        expired_at: BlockNumber,
        limit: BlockNumber,
    },
    #[error(
        "input data from block {input_block} is rejected as stale because it is older than the limit of {stale_limit}"
    )]
    StaleInputs {
        input_block: BlockNumber,
        stale_limit: BlockNumber,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum NodeId {
    Transaction(TransactionId),
    Batch(BatchId),
}

struct Mempool {
    // Describes the current inflight state of the mempool.
    // =============================================================================================
    accounts: HashMap<AccountId, (Word, NodeId)>,
    nullifiers: HashSet<Nullifier>,
    notes: HashMap<NoteId, NodeId>,

    txs: HashMap<TransactionId, AuthenticatedTransaction>,
    batches: HashMap<BatchId, ProvenBatch>,

    children: HashMap<NodeId, HashSet<NodeId>>,
    parents: HashMap<NodeId, HashSet<NodeId>>,

    // Mempool metadata and configuration
    // =============================================================================================
    block_budget: BlockBudget,
    batch_budget: BatchBudget,
    expiration_slack: u32,
    chain_tip: BlockNumber,
    /// Place holder until committed blocks are retained.
    oldest_state: BlockNumber,
}

/// Public functions.
impl Mempool {
    pub fn submit_transaction(
        &mut self,
        tx: AuthenticatedTransaction,
    ) -> Result<BlockNumber, SubmissionError> {
        self.check_expiration(tx.expires_at())?;
        self.check_input_staleness(tx.authentication_height())?;

        // Ensure state transition is valid _before_ mutating state. This ensures the function
        // remains atomic.
        let mut parents = HashSet::<NodeId>::default();

        let account_parent = self.check_account(
            tx.account_id(),
            tx.account_update().initial_state_commitment(),
            tx.store_account_state(),
        )?;
        parents.extend(account_parent);
        self.check_nullifiers(tx.nullifiers())?;
        self.check_output_notes(tx.output_note_ids())?;

        let note_parents = self.check_unauthenticated_notes(tx.unauthenticated_notes())?;
        parents.extend(note_parents);

        // Update state
        let id = NodeId::Transaction(tx.id());
        self.accounts
            .insert(tx.account_id(), (tx.account_update().final_state_commitment(), id));
        self.nullifiers.extend(tx.nullifiers());
        self.notes.extend(tx.output_note_ids().map(|note| (note, id)));
        self.txs.insert(tx.id(), tx);

        // Insert transaction node into graph.
        for parent in &parents {
            self.parents.get_mut(parent).unwrap().insert(id);
        }
        self.children.insert(id, HashSet::default());
        self.parents.insert(id, parents);

        // TODO: check whether this tx is available for selection.

        self.inject_telemetry();
        Ok(self.chain_tip)
    }

    pub fn submit_user_batch(
        &mut self,
        batch: ProvenBatch,
        input: (),
    ) -> Result<BlockNumber, SubmissionError> {
        self.check_expiration(batch.batch_expiration_block_num())?;
        self.check_input_staleness(todo!("get from batch input"))?;

        let mut accounts = HashSet::<AccountId>::default();

        let mut parents = HashSet::new();
        let mut children = HashSet::<NodeId>::new();
        let mut existing = HashSet::new();

        for tx in batch.transactions().as_slice() {
            // If the transaction already exists then its state is already accounted for in the
            // mempool state.
            if self.txs.contains_key(&tx.id()) {
                let id = NodeId::Transaction(tx.id());
                // Ensure that transaction is not already in a batch. Overlap with transactions
                // submitted via user batches are ignored as these will naturally result in a state
                // mismatch.
                if !self.parents.contains_key(&id) {
                    return Err(SubmissionError::AlreadyInBatch(tx.id()));
                }

                // TODO: think hard about whether this is sufficient..
                children.extend(self.children.get(&id).unwrap());
                parents.extend(self.parents.get(&id).unwrap());
                existing.insert(id);
            } else {
                if !accounts.contains(&tx.account_id()) {
                    let parent = self.check_account(
                        tx.account_id(),
                        tx.initial_state_commitment(),
                        todo!("store input"),
                    )?;
                    parents.extend(parent);

                    accounts.insert(tx.account_id());
                }

                self.check_nullifiers(tx.input_notes().iter().copied())?;
                self.check_output_notes(tx.output_notes().iter().copied())?;
            }
        }

        // TODO: check unauthenticated notes.. requires the store inputs

        // Remove internal edges from the batch's parents and children. This could happen if
        // existing transactions depend on each other.
        parents.retain(|x| !existing.contains(x));
        children.retain(|x| !existing.contains(x));

        // TODO: figure out how to check for external cycles aka batch children that connect to
        // parents. This would cause a cyclic depedency between this batch and the external nodes
        // and means they would all collectively need to be committed in the same block. This is
        // _not good_ and so we should detect and reject this. Somehow.

        // Remove output notes created by existing transactions as some of these would be ephemeral
        // notes consumed within the batch itself.
        for tx in &existing {
            let tx = self.txs.get(tx.as_transaction().unwrap()).unwrap();
            for note in tx.output_note_ids() {
                self.notes.remove(&note);
            }
        }
        // TODO: remove account updates

        // Insert state from batch
        let id = NodeId::Batch(batch.id());
        self.nullifiers.extend(batch.created_nullifiers());
        self.notes.extend(batch.output_notes().iter().map(|note| (note.id(), id)));
        // TODO: account commitment update..? :/ We cannot do a simple FIFO because we need to allow
        // collapsing multiple tx updates into a single batch update. So we need a linked list..
        // Solve this later when its more obvious..

        // TODO: update parents and children and the graph..

        //
        //
        self.parents.insert(id, parents);
        self.children.insert(id, children);
        self.batches.insert(batch.id(), batch);

        // TODO: figure out roots and batch checking. Don't forget to remove existing txs from roots

        self.inject_telemetry();

        Ok(self.chain_tip)
    }
}

/// Internal functions.
impl Mempool {
    fn check_expiration(&self, expires_at: BlockNumber) -> Result<(), SubmissionError> {
        if expires_at <= self.chain_tip + self.expiration_slack {
            return Err(SubmissionError::Expired {
                expired_at: expires_at,
                limit: self.chain_tip + self.expiration_slack,
            });
        } else {
            Ok(())
        }
    }

    fn check_input_staleness(
        &self,
        authentication_height: BlockNumber,
    ) -> Result<(), SubmissionError> {
        if authentication_height < self.oldest_state {
            return Err(SubmissionError::StaleInputs {
                input_block: authentication_height,
                stale_limit: self.oldest_state,
            });
        } else {
            Ok(())
        }
    }

    fn check_account(
        &self,
        account: AccountId,
        expected: Word,
        store: Option<Word>,
    ) -> Result<Option<NodeId>, SubmissionError> {
        let (current, parent) = self.accounts.get(&account).copied().unzip();
        let current = current.or(store).unwrap_or_default();

        if expected != current {
            Err(SubmissionError::IncorrectAccountCommitment { account, expected, current })
        } else {
            Ok(parent)
        }
    }

    fn check_nullifiers(
        &self,
        nullifiers: impl IntoIterator<Item = Nullifier>,
    ) -> Result<(), SubmissionError> {
        for nullifier in nullifiers {
            if self.nullifiers.contains(&nullifier) {
                return Err(SubmissionError::DoubleSpend(nullifier));
            }
        }
        Ok(())
    }

    fn check_output_notes(
        &self,
        notes: impl Iterator<Item = NoteId>,
    ) -> Result<(), SubmissionError> {
        for note in notes {
            if self.notes.contains_key(&note) {
                return Err(SubmissionError::DuplicateNote(note));
            }
        }
        Ok(())
    }

    fn check_unauthenticated_notes(
        &self,
        notes: impl Iterator<Item = NoteId>,
    ) -> Result<HashSet<NodeId>, SubmissionError> {
        let mut parents = HashSet::default();

        for note in notes {
            let Some(parent) = self.notes.get(&note) else {
                return Err(SubmissionError::UnknownNote(note));
            };
            parents.insert(*parent);
        }

        Ok(parents)
    }

    fn inject_telemetry(&self) {
        todo!();
    }
}

impl NodeId {
    fn as_transaction(&self) -> Option<&TransactionId> {
        match self {
            NodeId::Transaction(transaction_id) => transaction_id.into(),
            NodeId::Batch(_) => None,
        }
    }
}
