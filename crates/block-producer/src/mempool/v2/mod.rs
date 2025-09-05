#![allow(unreachable_code, dead_code, unused_variables, reason = "wip")]

use std::collections::HashMap;

use miden_objects::Word;
use miden_objects::account::AccountId;
use miden_objects::batch::{BatchId, ProvenBatch};
use miden_objects::block::BlockNumber;
use miden_objects::note::{NoteId, Nullifier};
use miden_objects::transaction::{TransactionHeader, TransactionId};

use crate::domain::batch::AuthenticatedBatch;
use crate::domain::transaction::AuthenticatedTransaction;
use crate::mempool::{BatchBudget, BlockBudget};

#[derive(Debug, thiserror::Error)]
enum SubmissionError {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum NodeId {
    Transaction(TransactionId),
    Batch(BatchId),
}

struct Mempool {
    state: state_graph::StateGraph,

    txs: HashMap<TransactionId, AuthenticatedTransaction>,
    batches: HashMap<BatchId, ProvenBatch>,

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
        tx: &AuthenticatedTransaction,
    ) -> Result<BlockNumber, SubmissionError> {
        // Separate state check from insertion to maintain atomicity.
        self.check_transaction(tx)?;

        let id = NodeId::Transaction(tx.id());
        self.state.insert(id, tx);

        self.inject_telemetry();
        Ok(self.chain_tip)
    }

    pub fn submit_user_batch(
        &mut self,
        batch: &AuthenticatedBatch,
    ) -> Result<BlockNumber, SubmissionError> {
        // Separate state check from insertion to maintain atomicity.
        self.check_batch(batch)?;

        // Remove existing transactions from the state graph as the batch is replacing them.
        //
        // This temporarily leaves the state in an inconsistent state, but that will be rectified
        // once the batch is inserted. This works because removing a node from the state only
        // removes the records of what this node created or consumed. Records of how other nodes
        // interact with this data remain intact.
        //
        // As an example, if the node created note A then this entry is removed, but the entry of
        // another node consuming note A is retained. This leaves the other node disconnected but
        // this will be corrected with a new entry for the batch's creation of note A.
        for tx in batch.transactions().iter().map(TransactionHeader::id) {
            if let Some(tx) = self.txs.get(&tx) {
                self.state.remove(tx);
            }
        }

        let id = NodeId::Batch(batch.id());
        self.state.insert(id, batch);

        self.inject_telemetry();

        Ok(self.chain_tip)
    }
}

/// Internal functions.
impl Mempool {
    /// Checks that the transaction's state transition is valid wrt to the current mempool state.
    ///
    /// If this check succeeds then it is safe to insert the transaction (given no other mutations
    /// occur before then).
    ///
    /// This function is intentionally separated out as a _immutable_ reference to signal that this
    /// check does not alter state.
    fn check_transaction(&self, tx: &AuthenticatedTransaction) -> Result<(), SubmissionError> {
        self.check_expiration(tx.expires_at())?;
        self.check_input_staleness(tx.authentication_height())?;

        self.check_tx_header(&tx.raw_proven_transaction().into(), tx.store_account_state())?;
        self.check_unauthenticated_input_notes(tx.unauthenticated_notes())
    }

    /// Checks that the transaction's state transition is valid wrt to the current mempool state.
    ///
    /// If this check succeeds then it is safe to insert the batch (given no other mutations
    /// occur before then).
    ///
    /// This function is intentionally separated out as a _immutable_ reference to signal that this
    /// check does not alter state.
    fn check_batch(&self, batch: &AuthenticatedBatch) -> Result<(), SubmissionError> {
        self.check_expiration(batch.expires_at())?;
        self.check_input_staleness(batch.authentication_height())?;

        // Check state but account for pre-existing transactions.
        //
        // We cannot only check at the batch level because the state already includes portions of
        // the batch's state from the pre-existing transactions.
        for tx in batch.transactions() {
            if self.txs.contains_key(&tx.id()) {
                continue;
            }

            self.check_tx_header(tx, batch.store_account_state(&tx.account_id()))?;
        }

        // This check does not need to exclude pre-existing transactions because the check only
        // tests for note existence and not whether they've already been consumed. Consumption
        // was implicitly checked via the tx header checks above which _do_ account for existing
        // transactions.
        self.check_unauthenticated_input_notes(batch.unauthenticated_notes())
    }

    /// Validates the state transition caused by this transaction.
    ///
    /// This includes checks for the account state transition, nullifier double spending and
    /// duplicate output note creation.
    fn check_tx_header(
        &self,
        tx: &TransactionHeader,
        store_state: Option<Word>,
    ) -> Result<(), SubmissionError> {
        let current = self
            .state
            .account_commitment(tx.account_id())
            .or(store_state)
            .unwrap_or_default();
        let expected = tx.initial_state_commitment();
        if current != expected {
            return Err(SubmissionError::IncorrectAccountCommitment {
                account: tx.account_id(),
                expected,
                current,
            });
        }

        for nullifier in tx.input_notes() {
            if self.state.nullifier_exists(nullifier) {
                return Err(SubmissionError::DoubleSpend(*nullifier));
            }
        }

        for note in tx.output_notes() {
            if self.state.note_exists(note) {
                return Err(SubmissionError::DuplicateNote(*note));
            }
        }

        Ok(())
    }

    /// Ensures that _all_ unauthenticated notes exist in the state. It _does not_ check that these
    /// notes are already consumed.
    fn check_unauthenticated_input_notes(
        &self,
        notes: impl Iterator<Item = NoteId>,
    ) -> Result<(), SubmissionError> {
        for note in notes {
            if !self.state.note_exists(&note) {
                return Err(SubmissionError::UnknownNote(note));
            }
        }
        Ok(())
    }

    fn check_expiration(&self, expires_at: BlockNumber) -> Result<(), SubmissionError> {
        if expires_at <= self.chain_tip + self.expiration_slack {
            return Err(SubmissionError::Expired {
                expired_at: expires_at,
                limit: self.chain_tip + self.expiration_slack,
            });
        }
        Ok(())
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
        }
        Ok(())
    }

    fn inject_telemetry(&self) {
        todo!();
    }
}

mod state_graph {
    use std::collections::hash_map::Entry;
    use std::collections::{HashMap, HashSet};

    use miden_objects::Word;
    use miden_objects::account::AccountId;
    use miden_objects::note::{NoteId, Nullifier};

    use crate::domain::batch::AuthenticatedBatch;
    use crate::domain::transaction::AuthenticatedTransaction;
    use crate::mempool::v2::NodeId;

    /// Maintains a view of the mempool's state and which node's produced or consumed a resource.
    ///
    /// This allows it to link nodes together into a DAG with parent <-> child relationships. These
    /// edges are not maintained directly but instead have one level of indirection using the state
    /// resources as an intermediery. This allows nodes in the graph to be removed and replaced
    /// without impacting connected nodes. This is useful to replace a set of transactions with a
    /// batch node for example.
    #[derive(Default)]
    pub struct StateGraph {
        accounts: HashMap<AccountId, AccountTransitions>,
        created_notes: HashMap<NoteId, NodeId>,
        consumed_notes: HashMap<NoteId, NodeId>,
        nullifiers: HashSet<Nullifier>,
    }

    /// Describes an account's state transitions and the node's which caused them.
    #[derive(Default)]
    struct AccountTransitions {
        /// Alias for an account's initial state.
        from: HashMap<Word, NodeId>,
        /// Alias for an account's final state.
        to: HashMap<Word, NodeId>,
    }

    /// Represents the data of a node in the state graph.
    ///
    /// Used to abstract over transactions and batches as both co-exist within the state graph.
    pub trait Node {
        fn nullifiers(&self) -> impl Iterator<Item = Nullifier>;
        fn account_updates(&self) -> impl Iterator<Item = (AccountId, Word, Word)>;
        fn unauthenticated_input_notes(&self) -> impl Iterator<Item = NoteId>;
        fn output_notes(&self) -> impl Iterator<Item = NoteId>;
    }

    impl StateGraph {
        pub fn nullifier_exists(&self, nullifier: &Nullifier) -> bool {
            self.nullifiers.contains(nullifier)
        }

        pub fn note_exists(&self, note: &NoteId) -> bool {
            self.created_notes.contains_key(note)
        }

        pub fn remove(&mut self, node: &impl Node) {
            for (account, from, to) in node.account_updates() {
                let Entry::Occupied(mut entry) = self.accounts.entry(account) else {
                    panic!("state is in shambles");
                };

                if entry.get_mut().remove(from, to) {
                    entry.remove();
                }
            }

            for nullifier in node.nullifiers() {
                self.nullifiers.remove(&nullifier);
            }

            for note in node.output_notes() {
                self.created_notes.remove(&note);
            }

            for note in node.unauthenticated_input_notes() {
                self.consumed_notes.remove(&note);
            }
        }

        /// Inserts a new node into the state graph.
        ///
        /// Caller is responsible for ensuring that state validity is maintained. This cannot be
        /// done internally because this is intended as a low-level function call, which can be
        /// used in conjunction with `remove` to e.g. replace a set of transaction's with a batch.
        pub fn insert(&mut self, id: NodeId, node: &impl Node) {
            for (account, from, to) in node.account_updates() {
                self.accounts.entry(account).or_default().insert(id, from, to);
            }

            for nullifier in node.nullifiers() {
                self.nullifiers.insert(nullifier);
            }

            for note in node.output_notes() {
                self.created_notes.insert(note, id);
            }

            for note in node.unauthenticated_input_notes() {
                self.consumed_notes.insert(note, id);
            }
        }

        /// The latest inflight state of the given account.
        pub fn account_commitment(&self, account: AccountId) -> Option<Word> {
            self.accounts.get(&account).and_then(AccountTransitions::commitment)
        }

        /// All descendents of the given node.
        ///
        /// This is derived directly from the state and is therefore only valid until the graph is
        /// mutated.
        pub fn children(&self, node: &impl Node) -> HashSet<NodeId> {
            let note_children =
                node.output_notes().filter_map(|note| self.consumed_notes.get(&note)).copied();

            let account_children = node
                .account_updates()
                .filter_map(|(account, _, to)| {
                    self.accounts.get(&account).unwrap().consumed_by(&to)
                })
                .copied();

            account_children.chain(note_children).collect()
        }

        /// All ancestors of the given node.
        ///
        /// This is derived directly from the state and is therefore only valid until the graph is
        /// mutated.
        pub fn parents(&self, node: &impl Node) -> HashSet<NodeId> {
            let note_parents = node
                .unauthenticated_input_notes()
                .filter_map(|note| self.created_notes.get(&note))
                .copied();

            let account_parents = node
                .account_updates()
                .filter_map(|(account, from, _)| {
                    self.accounts.get(&account).unwrap().created_by(&from)
                })
                .copied();

            account_parents.chain(note_parents).collect()
        }
    }

    impl AccountTransitions {
        fn insert(&mut self, node: NodeId, from: Word, to: Word) {
            assert!(
                self.from.insert(from, node).is_none(),
                "Initial account state inserted by {node:?} already exists"
            );
            assert!(
                self.to.insert(to, node).is_none(),
                "Final account state inserted by {node:?} already exists"
            );
        }

        fn remove(&mut self, from: Word, to: Word) -> bool {
            self.from.remove(&from);
            self.from.remove(&to);

            self.is_empty()
        }

        fn commitment(&self) -> Option<Word> {
            self.to.iter().find(|(c, _)| !self.from.contains_key(c)).map(|(c, _)| *c)
        }

        fn consumed_by(&self, to: &Word) -> Option<&NodeId> {
            self.from.get(to)
        }

        fn created_by(&self, from: &Word) -> Option<&NodeId> {
            self.to.get(from)
        }

        fn is_empty(&self) -> bool {
            self.from.is_empty() && self.to.is_empty()
        }
    }

    impl Node for AuthenticatedTransaction {
        fn nullifiers(&self) -> impl Iterator<Item = Nullifier> {
            self.nullifiers()
        }

        fn account_updates(&self) -> impl Iterator<Item = (AccountId, Word, Word)> {
            let update = self.account_update();

            std::iter::once((
                update.account_id(),
                update.initial_state_commitment(),
                update.final_state_commitment(),
            ))
        }

        fn unauthenticated_input_notes(&self) -> impl Iterator<Item = NoteId> {
            self.unauthenticated_notes()
        }

        fn output_notes(&self) -> impl Iterator<Item = NoteId> {
            self.output_note_ids()
        }
    }

    impl Node for AuthenticatedBatch {
        fn nullifiers(&self) -> impl Iterator<Item = Nullifier> {
            self.nullifiers()
        }

        fn account_updates(&self) -> impl Iterator<Item = (AccountId, Word, Word)> {
            self.account_updates().iter().map(|(account, update)| {
                (*account, update.initial_state_commitment(), update.final_state_commitment())
            })
        }

        fn unauthenticated_input_notes(&self) -> impl Iterator<Item = NoteId> {
            self.unauthenticated_notes()
        }

        fn output_notes(&self) -> impl Iterator<Item = NoteId> {
            self.output_note_ids()
        }
    }
}
