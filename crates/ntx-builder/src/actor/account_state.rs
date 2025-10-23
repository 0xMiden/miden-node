use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::num::NonZeroUsize;

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

use super::ActorShutdownReason;
use super::note_state::{NetworkAccountNoteState, NetworkAccountUpdate};
use crate::COMPONENT;
use crate::actor::inflight_note::InflightNetworkNote;
use crate::builder::ChainState;
use crate::store::{StoreClient, StoreError};

// TRANSACTION CANDIDATE
// ================================================================================================

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

// NETWORK ACCOUNT STATE
// ================================================================================================

/// The current state of a network account.
#[derive(Clone)]
pub struct NetworkAccountState {
    /// The network account prefix corresponding to the network account this state represents.
    account_prefix: NetworkAccountPrefix,

    /// Component of this state which Contains the committed and inflight account updates as well
    /// as available and nullified notes.
    account: NetworkAccountNoteState,

    /// Uncommitted transactions which have some impact on the network state.
    ///
    /// This is tracked so we can commit or revert such transaction effects. Transactions _without_
    /// an impact are ignored.
    inflight_txs: BTreeMap<TransactionId, TransactionImpact>,

    /// A set of nullifiers which have been registered for the network account.
    nullifier_idx: HashSet<Nullifier>,
}

impl NetworkAccountState {
    /// Maximum number of attempts to execute a network note.
    const MAX_NOTE_ATTEMPTS: usize = 30;

    /// Load's all available network notes from the store, along with the required account states.
    #[instrument(target = COMPONENT, name = "ntx.state.load", skip_all)]
    pub async fn load(
        account: Account,
        account_prefix: NetworkAccountPrefix,
        store: &StoreClient,
        block_num: BlockNumber,
    ) -> Result<Self, StoreError> {
        let notes = store.get_unconsumed_network_notes(account_prefix, block_num.as_u32()).await?;
        let notes = notes
            .into_iter()
            .filter_map(|note| {
                if let NetworkNote::SingleTarget(note) = note {
                    Some(note)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        let account = NetworkAccountNoteState::new(account, notes);

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
        chain_state: ChainState,
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
            .available_notes(&chain_state.chain_tip_header.block_num())
            .take(limit.get())
            .cloned()
            .collect::<Vec<_>>();

        // Skip accounts with no available notes.
        if notes.is_empty() {
            return None;
        }

        let (chain_tip_header, chain_mmr) = chain_state.into_parts();
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
                let network_notes =
                    filter_by_prefix_and_map_to_single_target(self.account_prefix, network_notes);
                self.add_transaction(id, nullifiers, network_notes, account_delta);
            },
            MempoolEvent::TransactionsReverted(txs) => {
                for tx in txs {
                    let shutdown_reason = self.revert_transaction(tx);
                    if shutdown_reason.is_some() {
                        return shutdown_reason;
                    }
                }
            },
            MempoolEvent::BlockCommitted { txs, .. } => {
                for tx in txs {
                    self.commit_transaction(tx);
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
                    NetworkAccountUpdate::Delta(account_delta) => {
                        self.account.add_delta(&account_delta);
                    },
                    NetworkAccountUpdate::New(_) => {},
                }
                tx_impact.account_delta = Some(account_prefix);
            }
        }
        for note in network_notes {
            assert_eq!(
                note.account_prefix(),
                self.account_prefix,
                "transaction note prefix does not match network account actor's prefix"
            );
            tx_impact.notes.insert(note.nullifier());
            self.nullifier_idx.insert(note.nullifier());
            self.account.add_note(note);
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

/// Filters network notes by prefix and maps them to single target network notes.
fn filter_by_prefix_and_map_to_single_target(
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
