use std::collections::{HashMap, HashSet};

use miden_objects::batch::{BatchId, ProvenBatch};
use miden_objects::block::BlockNumber;
use miden_objects::transaction::TransactionId;

use crate::domain::transaction::AuthenticatedTransaction;
use crate::mempool::state_dag::{NodeId, StateGraph};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Batches {
    user_batches: HashMap<BatchId, ProvenBatch>,
    proposed: HashMap<BatchId, (Vec<TransactionId>, BlockNumber)>,
    proven: HashMap<BatchId, ProvenBatch>,

    unprocessed: HashSet<BatchId>,
    candidates: HashSet<BatchId>,
}

impl Batches {
    pub fn insert(&mut self, id: BatchId, txs: &[AuthenticatedTransaction]) {
        let expires_at = txs
            .iter()
            .map(AuthenticatedTransaction::expires_at)
            .min()
            .unwrap_or(u32::MAX.into());
        let txs = txs.iter().map(|tx| tx.id()).collect();
        self.proposed.insert(id, (txs, expires_at));
        self.unprocessed.insert(id);
    }

    pub fn remove(&mut self, id: BatchId) -> Option<ProvenBatch> {
        self.user_batches.remove(&id);
        self.proposed.remove(&id);
        self.unprocessed.remove(&id);
        self.candidates.remove(&id);
        self.proven.remove(&id)
    }

    pub fn submit_proof(&mut self, proof: ProvenBatch, dag: &StateGraph) {
        let id = proof.id();

        // Its possible for proofs to arrive for untracked batches, or even for duplicate proofs
        // to arrive.
        //
        // The former could occur if the batch is reverted while the proof is being generated,
        // and if this same batch is proposed again, then multiple proofs will be inflight at
        // once.
        //
        // In other words, this is not an exceptional circumstance.
        if self.proposed.remove(&id).is_none() {
            return;
        }
        self.proven.insert(id, proof);
        self.candidacy_check(id, dag);
    }

    pub fn check_user_batches(&mut self, dag: &StateGraph) {
        'again: loop {
            let user_batches = self.user_batches.keys().copied().collect::<Vec<_>>();
            'next: for id in user_batches {
                let parents = dag.parents(id).expect("user batch should be present in state DAG");

                for parent in parents {
                    match parent {
                        NodeId::Transaction(_) => continue 'next,
                        NodeId::Batch(parent) if self.user_batches.contains_key(&parent) => {
                            continue 'next;
                        },
                        _ => {},
                    }
                }

                let batch = self
                    .user_batches
                    .remove(&id)
                    .expect("must exist as we're iterating over the keys");
                self.proven.insert(id, batch);
                self.candidacy_check(id, dag);
                continue 'again;
            }
        }
    }

    pub fn next_candidate(&mut self) -> Option<BatchCandidate<'_>> {
        self.candidates
            .iter()
            .next()
            .copied()
            .map(|batch| BatchCandidate::new(self, batch))
    }

    fn candidacy_check(&mut self, id: BatchId, dag: &StateGraph) {
        // Only proven batches can be considered for selection in a block.
        if !self.proven.contains_key(&id) {
            return;
        }

        let parents = dag.parents(id).expect("state DAG should contain batch candidate");
        for parent in parents {
            match parent {
                NodeId::Block(_) => {},
                NodeId::Batch(parent) if !self.unprocessed.contains(&parent) => {},
                _ => return,
            }
        }

        self.candidates.insert(id);
    }
}

pub struct BatchCandidate<'a> {
    origin: &'a mut Batches,
    candidate: BatchId,
}

impl<'a> BatchCandidate<'a> {
    fn new(origin: &'a mut Batches, candidate: BatchId) -> Self {
        Self { origin, candidate }
    }

    pub fn get(&self) -> &ProvenBatch {
        self.origin
            .proven
            .get(&self.candidate)
            .expect("all candidates are tracked and we have exclusive control of origin")
    }

    #[must_use]
    pub fn select(self, dag: &StateGraph) {
        self.origin.candidates.remove(&self.candidate);
        self.origin.unprocessed.remove(&self.candidate);

        let children = dag
            .children(self.candidate)
            .expect("state DAG should contain the batch candidate");

        for child in children {
            if let NodeId::Batch(child) = child {
                self.origin.candidacy_check(child, dag);
            }
        }
    }
}
