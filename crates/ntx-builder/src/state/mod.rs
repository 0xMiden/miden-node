use std::{collections::VecDeque, num::NonZeroUsize};

use account::AccountUpdate;
use miden_node_proto::domain::{account::NetworkAccountPrefix, note::NetworkNote};

mod account;
mod notes;

pub struct Candidate {
    pub account_deltas: VecDeque<AccountUpdate>,
    pub notes: Vec<NetworkNote>,
}

#[derive(Default)]
pub struct State {
    accounts: account::AccountStates,
    notes: notes::Notes,
}

impl State {
    pub fn mempool_update(&mut self, update: ()) {
        todo!("Wait for mempool event stream to merge");
    }

    pub fn select_candidate(&mut self, limit: NonZeroUsize) -> Option<Candidate> {
        let candidate = self.notes.select()?;
        let account_deltas = self.accounts.get(&candidate).cloned().unwrap_or_default();
        let notes = self.notes.get(&candidate).take(limit.get()).cloned().collect();

        Some(Candidate { account_deltas, notes })
    }

    pub fn candidate_failed(&mut self, candidate: NetworkAccountPrefix) {
        self.notes.deselect(candidate);
    }
}
