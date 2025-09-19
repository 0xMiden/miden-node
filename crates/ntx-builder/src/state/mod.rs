use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::num::NonZeroUsize;

use account::{AccountState, InflightNetworkNote, NetworkAccountUpdate};
use miden_node_proto::domain::account::NetworkAccountPrefix;
use miden_node_proto::domain::mempool::MempoolEvent;
use miden_node_proto::domain::note::{NetworkNote, SingleTargetNetworkNote};
use miden_node_utils::tracing::OpenTelemetrySpanExt;
use miden_objects::account::Account;
use miden_objects::account::delta::AccountUpdateDetails;
use miden_objects::block::{BlockHeader, BlockNumber};
use miden_objects::note::{Note, Nullifier};
use miden_objects::transaction::{PartialBlockchain, TransactionId};
use tracing::instrument;

use crate::COMPONENT;
use crate::actor::ActorShutdownReason;
use crate::store::{StoreClient, StoreError};

mod account;

// CONSTANTS
// =================================================================================================

/// The maximum number of blocks to keep in memory while tracking the chain tip.
const MAX_BLOCK_COUNT: usize = 4;

/// A candidate network transaction.
///
/// Contains the data pertaining to a specific network account which can be used to build a network
/// transaction.
#[derive(Clone, Debug)]
pub struct TransactionCandidate {
    /// The current inflight state of the account.
    pub account: Account,

    /// A set of notes addressed to this network account.
    pub notes: Vec<InflightNetworkNote>,

    /// The latest locally committed block header.
    ///
    /// This should be used as the reference block during transaction execution.
    pub chain_tip_header: BlockHeader,

    /// The chain MMR, which lags behind the tip by one block.
    pub chain_mmr: PartialBlockchain,
}

#[derive(Clone)]
pub struct State {
    account_prefix: NetworkAccountPrefix,
    account: AccountState,

    /// Uncommitted transactions which have a some impact on the network state.
    ///
    /// This is tracked so we can commit or revert such transaction effects. Transactions _without_
    /// an impact are ignored.
    inflight_txs: BTreeMap<TransactionId, TransactionImpact>,

    /// A mapping of network note's to their account.
    nullifier_idx: HashSet<Nullifier>,
}

impl State {
    /// Maximum number of attempts to execute a network note.
    const MAX_NOTE_ATTEMPTS: usize = 30;

    /// Load's all available network notes from the store, along with the required account states.
    #[instrument(target = COMPONENT, name = "ntx.state.load", skip_all)]
    pub async fn load(
        account_prefix: NetworkAccountPrefix,
        account: Account,
        store: StoreClient,
    ) -> Result<Self, StoreError> {
        // TODO: only get notes relevant to this account.
        let notes = store.get_unconsumed_network_notes().await?;
        let account = AccountState::new(account_prefix, account, notes);

        let state = Self {
            account,
            account_prefix,
            inflight_txs: BTreeMap::default(),
            nullifier_idx: HashSet::default(),
        };

        state.inject_telemetry();

        Ok(state)
    }

    /// Selects the next candidate network transaction.
    #[instrument(target = COMPONENT, name = "ntx.state.select_candidate", skip_all)]
    pub fn select_candidate(
        &mut self,
        limit: NonZeroUsize,
        chain_tip_header: BlockHeader,
        chain_mmr: PartialBlockchain,
    ) -> Option<TransactionCandidate> {
        // Remove notes that have failed too many times.
        self.account.drop_failing_notes(Self::MAX_NOTE_ATTEMPTS);

        // Skip empty accounts, and prune them.
        // This is how we keep the number of accounts bounded.
        if self.account.is_empty() {
            return None;
        }

        // Select notes from the account that can be consumed or are ready for a retry.
        let notes = self
            .account
            .available_notes(&chain_tip_header.block_num())
            .take(limit.get())
            .cloned()
            .collect::<Vec<_>>();

        // Skip accounts with no available notes.
        if notes.is_empty() {
            return None;
        }

        TransactionCandidate {
            account: self.account.latest_account(),
            notes,
            chain_tip_header,
            chain_mmr,
        }
        .into()
    }

    /// Marks notes of a previously selected candidate as failed.
    ///
    /// Does not remove the candidate from the in-progress pool.
    #[instrument(target = COMPONENT, name = "ntx.state.notes_failed", skip_all)]
    pub fn notes_failed(&mut self, notes: &[Note], block_num: BlockNumber) {
        let nullifiers = notes.iter().map(Note::nullifier).collect::<Vec<_>>();
        self.account.fail_notes(nullifiers.as_slice(), block_num);
    }

    /// Updates state with the mempool event.
    #[instrument(target = COMPONENT, name = "ntx.state.mempool_update", skip_all)]
    pub async fn mempool_update(&mut self, update: MempoolEvent) -> Option<ActorShutdownReason> {
        let span = tracing::Span::current();
        span.set_attribute("mempool_event.kind", update.kind());

        match update {
            MempoolEvent::TransactionAdded {
                id,
                nullifiers,
                network_notes,
                account_delta,
            } => {
                // Filter network notes relevant to this account.
                let network_notes = to_single_target_prefix(self.account_prefix, network_notes);
                self.add_transaction(id, nullifiers, network_notes, account_delta);
            },
            MempoolEvent::BlockCommitted { header, txs } => {
                // TODO: still need commit_transaction?
                //if header.prev_block_commitment() != self.chain_tip_header.commitment() {
                //    return Some(ActorShutdownReason::CommittedBlockMismatch {
                //        account_prefix: self.account_prefix,
                //        parent_block: header.block_num(),
                //        current_block: self.chain_tip_header.block_num(),
                //    });
                //}
                //self.update_chain_tip(header);
                //for tx in txs {
                //self.commit_transaction(tx);
                //}
            },
            MempoolEvent::TransactionsReverted(txs) => {
                for tx in txs {
                    let shutdown_reason = self.revert_transaction(tx);
                    if shutdown_reason.is_some() {
                        return shutdown_reason;
                    }
                }
            },
        }
        self.inject_telemetry();

        // No shutdown, continue running actor.
        None
    }

    /// Handles a [`MempoolEvent::TransactionAdded`] event.
    fn add_transaction(
        &mut self,
        id: TransactionId,
        nullifiers: Vec<Nullifier>,
        network_notes: Vec<SingleTargetNetworkNote>,
        account_delta: Option<AccountUpdateDetails>,
    ) {
        // Skip transactions we already know about.
        //
        // This can occur since both ntx builder and the mempool might inform us of the same
        // transaction. Once when it was submitted to the mempool, and once by the mempool event.
        if self.inflight_txs.contains_key(&id) {
            return;
        }

        let mut tx_impact = TransactionImpact::default();
        if let Some(update) = account_delta.and_then(NetworkAccountUpdate::from_protocol) {
            let account_prefix = update.prefix();
            if account_prefix == self.account_prefix {
                match update {
                    NetworkAccountUpdate::New(_) => {
                        // TODO(serge): double check what else was happening here before.
                        // Do nothing. The coordinator created this actor on this event.
                    },
                    NetworkAccountUpdate::Delta(account_delta) => {
                        self.account.add_delta(&account_delta);
                    },
                }
                tx_impact.account_delta = Some(account_prefix);
            }
        }
        for note in network_notes {
            if note.account_prefix() == self.account_prefix {
                tx_impact.notes.insert(note.nullifier());
                self.nullifier_idx.insert(note.nullifier());
                self.account.add_note(note);
            }
        }
        for nullifier in nullifiers {
            // Ignore nullifiers that aren't network note nullifiers.
            if !self.nullifier_idx.contains(&nullifier) {
                continue;
            }
            tx_impact.nullifiers.insert(nullifier);
            // We don't use the entry wrapper here because the account must already exist.
            self.account.add_nullifier(nullifier);
        }

        if !tx_impact.is_empty() {
            self.inflight_txs.insert(id, tx_impact);
        }
    }

    /// Handles [`MempoolEvent::BlockCommitted`] events.
    fn commit_transaction(&mut self, tx: TransactionId) {
        // We only track transactions which have an impact on the network state.
        let Some(impact) = self.inflight_txs.remove(&tx) else {
            return;
        };

        if let Some(prefix) = impact.account_delta {
            if prefix == self.account_prefix {
                self.account.commit_delta();
            }
        }

        for nullifier in impact.nullifiers {
            if self.nullifier_idx.remove(&nullifier) {
                // Its possible for the account to no longer exist if the transaction creating it
                // was reverted.
                self.account.commit_nullifier(nullifier);
            }
        }
    }

    /// Handles [`MempoolEvent::TransactionsReverted`] events.
    fn revert_transaction(&mut self, tx: TransactionId) -> Option<ActorShutdownReason> {
        // We only track transactions which have an impact on the network state.
        let Some(impact) = self.inflight_txs.remove(&tx) else {
            tracing::debug!("transaction {tx} not found in inflight transactions");
            return None;
        };

        // Revert account creation.
        if let Some(account_prefix) = impact.account_delta {
            // Account creation reverted, actor must stop.
            if account_prefix == self.account_prefix && self.account.revert_delta() {
                return Some(ActorShutdownReason::AccountReverted(account_prefix));
            }
        }

        // Revert notes.
        for note_nullifier in impact.notes {
            if self.nullifier_idx.contains(&note_nullifier) {
                self.account.revert_note(note_nullifier);
                self.nullifier_idx.remove(&note_nullifier);
            }
        }

        // Revert nullifiers.
        for nullifier in impact.nullifiers {
            if self.nullifier_idx.contains(&nullifier) {
                self.account.revert_nullifier(nullifier);
                self.nullifier_idx.remove(&nullifier);
            }
        }

        None
    }

    /// Adds stats to the current tracing span.
    ///
    /// Note that these are only visible in the OpenTelemetry context, as conventional tracing
    /// does not track fields added dynamically.
    fn inject_telemetry(&self) {
        let span = tracing::Span::current();

        span.set_attribute("ntx.state.transactions", self.inflight_txs.len());
        span.set_attribute("ntx.state.notes.total", self.nullifier_idx.len());
    }
}

/// The impact a transaction has on the state.
#[derive(Clone, Default)]
struct TransactionImpact {
    /// The network account this transaction added an account delta to.
    account_delta: Option<NetworkAccountPrefix>,

    /// Network notes this transaction created.
    notes: BTreeSet<Nullifier>,

    /// Network notes this transaction consumed.
    nullifiers: BTreeSet<Nullifier>,
}

impl TransactionImpact {
    fn is_empty(&self) -> bool {
        self.account_delta.is_none() && self.notes.is_empty() && self.nullifiers.is_empty()
    }
}

fn to_single_target_prefix(
    account_prefix: NetworkAccountPrefix,
    notes: Vec<NetworkNote>,
) -> Vec<SingleTargetNetworkNote> {
    notes
        .into_iter()
        .filter_map(|note| match note {
            NetworkNote::SingleTarget(note) if note.account_prefix() == account_prefix => {
                Some(note)
            },
            _ => None,
        })
        .collect::<Vec<_>>()
}
