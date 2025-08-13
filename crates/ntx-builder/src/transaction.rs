use std::collections::BTreeSet;
use std::sync::Arc;

use miden_node_utils::ErrorReport;
use miden_node_utils::tracing::OpenTelemetrySpanExt;
use miden_objects::account::{Account, AccountId};
use miden_objects::assembly::DefaultSourceManager;
use miden_objects::block::{BlockHeader, BlockNumber};
use miden_objects::transaction::{
    ExecutedTransaction,
    InputNote,
    InputNotes,
    PartialBlockchain,
    ProvenTransaction,
    TransactionArgs,
};
use miden_objects::vm::FutureMaybeSend;
use miden_objects::{TransactionInputError, Word};
use miden_remote_prover_client::remote_prover::tx_prover::RemoteTransactionProver;
use miden_tx::auth::UnreachableAuth;
use miden_tx::{
    DataStore,
    DataStoreError,
    LocalTransactionProver,
    MastForestStore,
    NoteConsumptionChecker,
    NoteConsumptionInfo,
    TransactionExecutor,
    TransactionExecutorError,
    TransactionMastStore,
    TransactionProverError,
};
use rand::seq::SliceRandom;
use tokio::task::JoinError;
use tracing::{Instrument, instrument};

use crate::COMPONENT;
use crate::block_producer::BlockProducerClient;
use crate::state::TransactionCandidate;

#[derive(Debug, thiserror::Error)]
pub enum NtxError {
    #[error("note inputs were invalid")]
    InputNotes(#[source] TransactionInputError),
    #[error("failed to filter notes")]
    NoteFilter(#[source] TransactionExecutorError),
    #[error("no viable notes")]
    NoViableNotes,
    #[error("failed to execute transaction")]
    Execution(#[source] TransactionExecutorError),
    #[error("failed to prove transaction")]
    Proving(#[source] TransactionProverError),
    #[error("failed to submit transaction")]
    Submission(#[source] tonic::Status),
    #[error("the ntx task panic'd")]
    Panic(#[source] JoinError),
}

type NtxResult<T> = Result<T, NtxError>;

// Context and execution of network transactions
// ================================================================================================

/// Provides the context for execution [network transaction candidates](TransactionCandidate).
#[derive(Clone)]
pub struct NtxContext {
    pub block_producer: BlockProducerClient,

    /// The prover to delegate proofs to.
    ///
    /// Defaults to local proving if unset. This should be avoided in production as this is
    /// computationally intensive.
    pub prover: Option<RemoteTransactionProver>,
}

impl NtxContext {
    #[instrument(target = COMPONENT, name = "ntx.execute_transaction", skip_all, err)]
    pub fn execute_transaction(
        self,
        tx: TransactionCandidate,
    ) -> impl FutureMaybeSend<NtxResult<()>> {
        let TransactionCandidate {
            account,
            notes,
            chain_tip_header,
            chain_mmr,
        } = tx;
        tracing::Span::current().set_attribute("account.id", account.id());
        tracing::Span::current().set_attribute("notes.count", notes.len());
        tracing::Span::current()
            .set_attribute("reference_block.number", chain_tip_header.block_num());

        async move {
            async move {
                let mut notes = notes
                    .into_iter()
                    .map(|note| InputNote::Unauthenticated { note: note.into() })
                    .collect::<Vec<_>>();
                // We shuffle the notes here to prevent having a failing note always in
                // front.
                notes.shuffle(&mut rand::rng());
                let notes = InputNotes::new(notes).map_err(NtxError::InputNotes)?;

                let data_store = NtxDataStore::new(account, chain_tip_header, chain_mmr);

                let notes = Box::pin(self.filter_notes(&data_store, notes)).await?;
                let executed = Box::pin(self.execute(&data_store, notes)).await?;
                let proven = Box::pin(self.prove(executed)).await?;
                self.submit(proven).await?;
                Ok(())
            }
            .in_current_span()
            .await
            .inspect_err(|err| tracing::Span::current().set_error(err))
        }
    }

    /// Returns a set of input notes which can be successfully executed against the network account.
    ///
    /// The returned set is guaranteed to be non-empty.
    ///
    /// # Errors
    ///
    /// Returns an error if
    /// - execution fails unexpectedly
    /// - no notes are viable
    #[instrument(target = COMPONENT, name = "ntx.execute_transaction.filter_notes", skip_all, err)]
    async fn filter_notes(
        &self,
        data_store: &NtxDataStore,
        notes: InputNotes<InputNote>,
    ) -> NtxResult<InputNotes<InputNote>> {
        let executor: TransactionExecutor<'_, '_, _, UnreachableAuth> =
            TransactionExecutor::new(data_store, None);
        let checker = NoteConsumptionChecker::new(&executor);

        let notes = match Box::pin(checker.check_notes_consumability(
            data_store.account.id(),
            data_store.reference_header.block_num(),
            notes.clone(),
            TransactionArgs::default(),
        ))
        .await
        {
            Ok(NoteConsumptionInfo { successful, failed, .. }) => {
                let notes = successful
                    .into_iter()
                    .filter_map(|success| notes.iter().find(|note| note.id() == success.id()))
                    .cloned()
                    .collect::<Vec<InputNote>>();
                // TODO revisit verbosity once we scale up to many more notes
                for failed in failed {
                    let err = failed.error.as_report();
                    let note_id = failed.note.id();
                    tracing::warn!(%note_id, %err, "note failed");
                }
                InputNotes::new_unchecked(notes)
            },
            Err(err) => return Err(NtxError::NoteFilter(err)),
        };

        if notes.is_empty() {
            Err(NtxError::NoViableNotes)
        } else {
            Ok(notes)
        }
    }

    /// Creates an executes a transaction with the network account and the given set of notes.
    #[instrument(target = COMPONENT, name = "ntx.execute_transaction.execute", skip_all, err)]
    async fn execute(
        &self,
        data_store: &NtxDataStore,
        notes: InputNotes<InputNote>,
    ) -> NtxResult<ExecutedTransaction> {
        let executor: TransactionExecutor<'_, '_, _, UnreachableAuth> =
            TransactionExecutor::new(data_store, None);

        Box::pin(executor.execute_transaction(
            data_store.account.id(),
            data_store.reference_header.block_num(),
            notes,
            TransactionArgs::default(),
            Arc::new(DefaultSourceManager::default()),
        ))
        .await
        .map_err(NtxError::Execution)
    }

    /// Delegates the transaction proof to the remote prover if configured, otherwise performs the
    /// proof locally.
    #[instrument(target = COMPONENT, name = "ntx.execute_transaction.prove", skip_all, err)]
    async fn prove(&self, tx: ExecutedTransaction) -> NtxResult<ProvenTransaction> {
        if let Some(remote) = &self.prover {
            remote.prove(tx.into()).await
        } else {
            tokio::task::spawn_blocking(move || LocalTransactionProver::default().prove(tx.into()))
                .await
                .map_err(NtxError::Panic)?
        }
        .map_err(NtxError::Proving)
    }

    /// Submits the transaction to the block producer.
    #[instrument(target = COMPONENT, name = "ntx.execute_transaction.submit", skip_all, err)]
    async fn submit(&self, tx: ProvenTransaction) -> NtxResult<()> {
        self.block_producer
            .submit_proven_transaction(tx)
            .await
            .map_err(NtxError::Submission)
    }
}

// Data store implementation for the transaction execution
// ================================================================================================

/// A [`DataStore`] implementation which provides transaction inputs for a single account and
/// reference block.
///
/// This is sufficient for executing a network transaction.
struct NtxDataStore {
    account: Account,
    reference_header: BlockHeader,
    chain_mmr: PartialBlockchain,
    mast_store: TransactionMastStore,
}

impl NtxDataStore {
    fn new(account: Account, reference_header: BlockHeader, chain_mmr: PartialBlockchain) -> Self {
        let mast_store = TransactionMastStore::new();
        mast_store.load_account_code(account.code());

        Self {
            account,
            reference_header,
            chain_mmr,
            mast_store,
        }
    }
}

impl DataStore for NtxDataStore {
    fn get_transaction_inputs(
        &self,
        account_id: AccountId,
        ref_blocks: BTreeSet<BlockNumber>,
    ) -> impl FutureMaybeSend<
        Result<(Account, Option<Word>, BlockHeader, PartialBlockchain), DataStoreError>,
    > {
        let account = self.account.clone();
        async move {
            if account.id() != account_id {
                return Err(DataStoreError::AccountNotFound(account_id));
            }

            match ref_blocks.last().copied() {
                Some(reference) if reference == self.reference_header.block_num() => {},

                Some(other) => return Err(DataStoreError::BlockNotFound(other)),
                None => return Err(DataStoreError::other("no reference block requested")),
            }

            Ok((account, None, self.reference_header.clone(), self.chain_mmr.clone()))
        }
    }
}

impl MastForestStore for NtxDataStore {
    fn get(
        &self,
        procedure_hash: &miden_objects::Word,
    ) -> Option<std::sync::Arc<miden_objects::MastForest>> {
        self.mast_store.get(procedure_hash)
    }
}
