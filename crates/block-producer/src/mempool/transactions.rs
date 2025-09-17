use std::collections::{HashMap, HashSet};

use miden_objects::block::BlockNumber;
use miden_objects::transaction::TransactionId;

use crate::domain::transaction::AuthenticatedTransaction;
use crate::mempool::state_dag::{NodeId, StateGraph};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Transactions {
    raw: HashMap<TransactionId, AuthenticatedTransaction>,
    unprocessed: HashSet<TransactionId>,
    candidates: HashSet<TransactionId>,
}

impl Transactions {
    pub fn insert(&mut self, tx: AuthenticatedTransaction, dag: &StateGraph) {
        let id = tx.id();
        self.raw.insert(id, tx);
        self.candidacy_check(id, dag);
    }

    pub fn remove(&mut self, tx: TransactionId) {
        self.raw.remove(&tx);
        self.unprocessed.remove(&tx);
        self.candidates.remove(&tx);
    }

    pub fn next_candidate(&mut self) -> Option<TransactionCandidate<'_>> {
        self.candidates
            .iter()
            .next()
            .copied()
            .map(|tx| TransactionCandidate::new(self, tx))
    }

    pub fn expired(&mut self, chain_tip: BlockNumber) -> HashSet<TransactionId> {
        self.raw
            .values()
            .filter_map(|tx| (tx.expires_at() >= chain_tip).then_some(tx.id()))
            .collect()
    }

    fn candidacy_check(&mut self, tx: TransactionId, dag: &StateGraph) {
        let parents = dag.parents(tx).expect("state DAG should contain transaction candidate");

        for parent in parents {
            match parent {
                NodeId::Transaction(child) if self.unprocessed.contains(&child) => {
                    return;
                },
                NodeId::Batch(_) => return,
                _ => {},
            }
        }

        self.candidates.insert(tx);
    }
}

pub struct TransactionCandidate<'a> {
    origin: &'a mut Transactions,
    candidate: TransactionId,
}

impl<'a> TransactionCandidate<'a> {
    fn new(origin: &'a mut Transactions, candidate: TransactionId) -> Self {
        Self { origin, candidate }
    }

    pub fn get(&self) -> &AuthenticatedTransaction {
        self.origin
            .raw
            .get(&self.candidate)
            .expect("all candidates are tracked and we have exclusive control of origin")
    }

    pub fn select(self, dag: &StateGraph) {
        self.origin.unprocessed.remove(&self.candidate);
        self.origin.candidates.remove(&self.candidate);

        let children =
            dag.children(self.candidate).expect("state DAG should contain the tx candidate");

        for child in children {
            if let NodeId::Transaction(child) = child {
                self.origin.candidacy_check(child, dag);
            }
        }
    }
}
