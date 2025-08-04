use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque, hash_map::Entry},
    num::NonZeroUsize,
};

use account::{AccountState, InflightNetworkNote, NetworkAccountUpdate};
use anyhow::Context;
use miden_node_proto::domain::{
    account::NetworkAccountPrefix, mempool::MempoolEvent, note::NetworkNote,
};
use miden_node_utils::tracing::OpenTelemetrySpanExt;
use miden_objects::{
    account::{Account, delta::AccountUpdateDetails},
    block::{BlockHeader, BlockNumber},
    note::Nullifier,
    transaction::{PartialBlockchain, TransactionId},
};
use tracing::instrument;

use crate::{
    COMPONENT,
    store::{StoreClient, StoreError},
};

mod account;

// CONSTANTS
// =================================================================================================

/// The maximum number of blocks to keep in memory while tracking the chain tip.
const MAX_BLOCK_COUNT: usize = 4;

/// A candidate network transaction.
///
/// Contains the data pertaining to a specific network account which can be used to build a network
/// transaction.
#[derive(Clone)]
pub struct TransactionCandidate {
    /// The current inflight state of the account.
    pub account: Account,

    /// The account id prefix corresponding to the candidate account.
    pub account_id_prefix: NetworkAccountPrefix,

    /// A set of notes addressed to this network account.
    pub notes: Vec<InflightNetworkNote>,

    /// The latest locally committed block header.
    ///
    /// This should be used as the reference block during transaction execution.
    pub chain_tip_header: BlockHeader,

    /// The chain MMR, which lags behind the tip by one block.
    pub chain_mmr: PartialBlockchain,
}

/// Holds the state of the network transaction builder.
///
/// It tracks inflight transactions, and their impact on network-related state.
pub struct State {
    /// The latest committed block header.
    chain_tip_header: BlockHeader,

    /// The chain MMR, which lags behind the tip by one block.
    chain_mmr: PartialBlockchain,

    /// Tracks all network accounts with inflight state.
    ///
    /// This is network account deltas, network notes and their nullifiers.
    accounts: HashMap<NetworkAccountPrefix, AccountState>,

    /// A rotating queue of all tracked network accounts.
    ///
    /// This is used to select the next transaction's account.
    ///
    /// Note that this _always_ includes _all_ network accounts. Filtering out accounts that aren't
    /// viable is handled within the select method itself.
    queue: VecDeque<NetworkAccountPrefix>,

    /// Network accounts which have been selected but whose transaction has not yet completed.
    ///
    /// This locks these accounts so they cannot be selected.
    in_progress: HashSet<NetworkAccountPrefix>,

    /// Uncommitted transactions which have a some impact on the network state.
    ///
    /// This is tracked so we can commit or revert such transaction effects. Transactions _without_
    /// an impact are ignored.
    inflight_txs: BTreeMap<TransactionId, TransactionImpact>,

    /// A mapping of network note's to their account.
    nullifier_idx: BTreeMap<Nullifier, NetworkAccountPrefix>,

    /// gRPC client used to retrieve the network account state from the store.
    store: StoreClient,
}

impl State {
    /// Maximum number of attempts to execute a network note.
    const MAX_NOTE_ATTEMPTS: usize = 30;

    /// Load's all available network notes from the store, along with the required account states.
    #[instrument(target = COMPONENT, name = "ntx.state.load", skip_all)]
    pub async fn load(store: StoreClient) -> Result<Self, StoreError> {
        let (chain_tip_header, chain_mmr) = store
            .get_latest_blockchain_data_with_retry()
            .await?
            .expect("store should contain a latest block");

        let chain_mmr = PartialBlockchain::new(chain_mmr, [])
            .expect("PartialBlockchain should build from latest partial MMR");

        let mut state = Self {
            chain_tip_header,
            chain_mmr,
            store,
            accounts: HashMap::default(),
            queue: VecDeque::default(),
            in_progress: HashSet::default(),
            inflight_txs: BTreeMap::default(),
            nullifier_idx: BTreeMap::default(),
        };

        let notes = state.store.get_unconsumed_network_notes().await?;
        for note in notes {
            let prefix = note.account_prefix();

            // Ignore notes which don't target an existing account.
            let Some(account) = state.fetch_account(prefix).await? else {
                continue;
            };
            account.add_note(note);
        }
        state.inject_telemetry();

        Ok(state)
    }

    /// Selects the next candidate network transaction.
    ///
    /// Note that this marks the candidate account as in-progress and that it cannot be selected
    /// again until either:
    ///
    ///   - it has been marked as failed if the transaction failed, or
    ///   - the transaction was submitted successfully, indicated by the associated mempool event
    ///     being submitted
    #[instrument(target = COMPONENT, name = "ntx.state.select_candidate", skip_all)]
    pub fn select_candidate(&mut self, limit: NonZeroUsize) -> Option<TransactionCandidate> {
        // Loop through the account queue until we find one that is selectable.
        //
        // Since the queue contains _all_ accounts, including unselectable accounts, we limit our
        // search to once through the entire queue.
        //
        // There are smarter ways of doing this, but this should scale more than well enough for a
        // long time.
        for _ in 0..self.queue.len() {
            // This is a rotating queue.
            let candidate = self.queue.pop_front().unwrap();
            self.queue.push_back(candidate);

            // Skip accounts which are already in-progress.
            if self.in_progress.contains(&candidate) {
                continue;
            }

            let account = self.accounts.get_mut(&candidate).expect("queue account must be tracked");

            // Remove notes that have failed too many times.
            account.drain_failing_notes(Self::MAX_NOTE_ATTEMPTS);

            // Skip empty accounts, and prune them.
            // This is how we keep the number of accounts bounded.
            if account.is_empty() {
                // We don't need to prune the inflight transactions because if the account is empty,
                // then it would have no inflight txs.
                self.accounts.remove(&candidate);
                // We know this account is the backmost one since we just rotated it there.
                self.queue.pop_back();
                continue;
            }

            // Select notes from the account that can be consumed or are ready for a retry.
            let notes = account
                .notes()
                .filter(|&note| note.is_available(self.chain_tip_header.block_num()))
                .take(limit.get())
                .cloned()
                .collect::<Vec<_>>();

            // Skip accounts with no available notes.
            if notes.is_empty() {
                continue;
            }

            self.in_progress.insert(candidate);
            return TransactionCandidate {
                account: account.latest_account(),
                account_id_prefix: candidate,
                notes,
                chain_tip_header: self.chain_tip_header.clone(),
                chain_mmr: self.chain_mmr.clone(),
            }
            .into();
        }
        self.inject_telemetry();

        None
    }

    /// The latest block number the state knows of.
    pub fn chain_tip(&self) -> BlockNumber {
        self.chain_tip_header.block_num()
    }

    /// Updates the chain tip and MMR block count.
    ///
    /// Blocks in the MMR are pruned if the block count exceeds the maximum.
    fn update_chain_tip(&mut self, tip: BlockHeader) {
        // Update MMR which lags by one block.
        self.chain_mmr.add_block(self.chain_tip_header.clone(), true);

        // Set the new tip.
        self.chain_tip_header = tip;

        // Keep MMR pruned.
        let pruned_block_height =
            (self.chain_mmr.chain_length().as_usize().saturating_sub(MAX_BLOCK_COUNT)) as u32;
        self.chain_mmr.prune_to(..pruned_block_height.into());
    }

    /// Marks a previously selected candidate account as failed, allowing it to be available for
    /// selection again.
    ///
    /// All notes in the candidate will be marked as failed.
    #[instrument(target = COMPONENT, name = "ntx.state.candidate_failed", skip_all)]
    pub fn candidate_failed(&mut self, candidate: &TransactionCandidate) {
        if let Some(account) = self.accounts.get_mut(&candidate.account_id_prefix) {
            account.fail(&candidate.notes, candidate.chain_tip_header.block_num());
        } else {
            tracing::error!(account.prefix=%candidate.account_id_prefix, "failed network transaction has no local account state");
        }

        self.in_progress.remove(&candidate.account_id_prefix);

        self.inject_telemetry();
    }

    /// Updates state with the mempool event.
    #[instrument(target = COMPONENT, name = "ntx.state.mempool_update", skip_all)]
    pub async fn mempool_update(&mut self, update: MempoolEvent) -> anyhow::Result<()> {
        let span = tracing::Span::current();
        span.set_attribute("mempool_event.kind", update.kind());

        match update {
            // Note: this event will get triggered by normal user transactions, as well as our
            // network transactions. The mempool does not distinguish between the two.
            MempoolEvent::TransactionAdded {
                id,
                nullifiers,
                network_notes,
                account_delta,
            } => {
                self.add_transaction(id, nullifiers, network_notes, account_delta).await?;
            },
            MempoolEvent::BlockCommitted { header, txs } => {
                anyhow::ensure!(
                    header.prev_block_commitment() == self.chain_tip_header.commitment(),
                    "New block's parent commitment {} does not match local chain tip {}",
                    header.prev_block_commitment(),
                    self.chain_tip_header.commitment()
                );
                self.update_chain_tip(header);
                for tx in txs {
                    self.commit_transaction(tx);
                }
            },
            MempoolEvent::TransactionsReverted(txs) => {
                for tx in txs {
                    self.revert_transaction(tx);
                }
            },
        }
        self.inject_telemetry();

        Ok(())
    }

    /// Handles a [`MempoolEvent::TransactionAdded`] event.
    ///
    /// Note that this will include our own network transactions as well as user submitted
    /// transactions.
    ///
    /// This updates the state of network accounts affected by this transaction. Account state
    /// may be loaded from the store if it isn't already known locally. This would be the case if
    /// the network account has no inflight state changes.
    async fn add_transaction(
        &mut self,
        id: TransactionId,
        nullifiers: Vec<Nullifier>,
        network_notes: Vec<NetworkNote>,
        account_delta: Option<AccountUpdateDetails>,
    ) -> anyhow::Result<()> {
        // Skip transactions we already know about.
        //
        // This can occur since both ntx builder and the mempool might inform us of the same
        // transaction. Once when it was submitted to the mempool, and once by the mempool event.
        if self.inflight_txs.contains_key(&id) {
            return Ok(());
        }

        let mut tx_impact = TransactionImpact::default();
        if let Some(update) = account_delta.and_then(NetworkAccountUpdate::from_protocol) {
            let prefix = update.prefix();
            match update {
                NetworkAccountUpdate::New(account) => {
                    let account_state = AccountState::from_uncommitted_account(account);
                    self.accounts.insert(prefix, account_state);
                    self.queue.push_back(prefix);
                },
                NetworkAccountUpdate::Delta(account_delta) => {
                    self.fetch_account(prefix)
                        .await
                        .context("failed to load account")?
                        .context("account with delta not found")?
                        .add_delta(&account_delta);
                },
            }

            // If this account was in-progress, then it should no longer be as this update is the
            // result of our own network transaction.
            self.in_progress.remove(&prefix);
            tx_impact.account_delta = Some(prefix);
        }
        for note in network_notes {
            tx_impact.notes.insert(note.nullifier());
            self.nullifier_idx.insert(note.nullifier(), note.account_prefix());
            // Skip notes which target a non-existent network account.
            if let Some(account) = self
                .fetch_account(note.account_prefix())
                .await
                .context("failed to load account")?
            {
                account.add_note(note);
            }
        }
        for nullifier in nullifiers {
            // Ignore nullifiers that aren't network note nullifiers.
            let Some(account) = self.nullifier_idx.get(&nullifier) else {
                continue;
            };
            tx_impact.nullifiers.insert(nullifier);
            // We don't use the entry wrapper here because the account must already exist.
            self.accounts
                .get_mut(account)
                .expect("nullifier account must exist")
                .add_nullifier(nullifier);
        }

        if !tx_impact.is_empty() {
            self.inflight_txs.insert(id, tx_impact);
        }

        Ok(())
    }

    /// Handles [`MempoolEvent::BlockCommitted`] events.
    fn commit_transaction(&mut self, tx: TransactionId) {
        // We only track transactions which have an impact on the network state.
        let Some(impact) = self.inflight_txs.remove(&tx) else {
            return;
        };

        if let Some(prefix) = impact.account_delta {
            self.accounts.get_mut(&prefix).unwrap().commit_delta();
        }

        for nullifier in impact.nullifiers {
            let prefix = self.nullifier_idx.remove(&nullifier).unwrap();
            // Its possible for the account to no longer exist if the transaction creating it was
            // reverted.
            if let Some(account) = self.accounts.get_mut(&prefix) {
                account.commit_nullifier(nullifier);
            }
        }
    }

    /// Handles [`MempoolEvent::TransactionsReverted`] events.
    fn revert_transaction(&mut self, tx: TransactionId) {
        // We only track transactions which have an impact on the network state.
        let Some(impact) = self.inflight_txs.remove(&tx) else {
            return;
        };

        if let Some(prefix) = impact.account_delta {
            // We need to remove the account if this transaction created the account.
            if self.accounts.get_mut(&prefix).unwrap().revert_delta() {
                self.accounts.remove(&prefix);
            }
        }

        for note in impact.notes {
            let prefix = self.nullifier_idx.remove(&note).unwrap();
            // Its possible for the account to no longer exist if the transaction creating it was
            // reverted.
            if let Some(account) = self.accounts.get_mut(&prefix) {
                account.revert_note(note);
            }
        }

        for nullifier in impact.nullifiers {
            let prefix = self.nullifier_idx.get(&nullifier).unwrap();
            // Its possible for the account to no longer exist if the transaction creating it was
            // reverted.
            if let Some(account) = self.accounts.get_mut(prefix) {
                account.revert_nullifier(nullifier);
            }
        }
    }

    /// Returns the current inflight account, loading it from the store if it isn't present locally.
    ///
    /// Returns `None` if the account is unknown.
    async fn fetch_account(
        &mut self,
        prefix: NetworkAccountPrefix,
    ) -> Result<Option<&mut AccountState>, StoreError> {
        match self.accounts.entry(prefix) {
            Entry::Occupied(occupied_entry) => Ok(Some(occupied_entry.into_mut())),
            Entry::Vacant(vacant_entry) => {
                let Some(account) = self.store.get_network_account(prefix).await? else {
                    return Ok(None);
                };

                self.queue.push_back(prefix);
                let entry = vacant_entry.insert(AccountState::from_committed_account(account));

                Ok(Some(entry))
            },
        }
    }

    /// Adds stats to the current tracing span.
    ///
    /// Note that these are only visible in the OpenTelemetry context, as conventional tracing
    /// does not track fields added dynamically.
    fn inject_telemetry(&self) {
        let span = tracing::Span::current();

        span.set_attribute("ntx.state.accounts.total", self.accounts.len());
        span.set_attribute("ntx.state.accounts.in_progress", self.in_progress.len());
        span.set_attribute("ntx.state.transactions", self.inflight_txs.len());
        span.set_attribute("ntx.state.notes.total", self.nullifier_idx.len());
    }
}

/// The impact a transaction has on the state.
#[derive(Default)]
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
