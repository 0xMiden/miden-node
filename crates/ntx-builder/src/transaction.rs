use std::collections::BTreeSet;

use miden_node_utils::tracing::OpenTelemetrySpanExt;
use miden_objects::account::{Account, AccountId};
use miden_objects::block::{BlockHeader, BlockNumber};
use miden_objects::note::Note;
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
    FailedNote,
    LocalTransactionProver,
    MastForestStore,
    NoteCheckerError,
    NoteConsumptionChecker,
    NoteConsumptionInfo,
    TransactionExecutor,
    TransactionExecutorError,
    TransactionMastStore,
    TransactionProverError,
};
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
    NoteFilter(#[source] NoteCheckerError),
    #[error("all notes failed to be executed")]
    AllNotesFailed(Vec<FailedNote>),
    #[error("failed to execute transaction")]
    Execution(#[source] TransactionExecutorError),
    #[error("failed to prove transaction")]
    Proving(#[source] TransactionProverError),
    #[error("failed to submit transaction")]
    Submission(#[source] tonic::Status),
    #[error("the ntx task panicked")]
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
    /// Executes a transaction end-to-end: filtering, executing, proving, and submitted to the block
    /// producer.
    ///
    /// The provided [`TransactionCandidate`] is processed in the following stages:
    /// 1. Note filtering – all input notes are checked for consumability. Any notes that cannot be
    ///    executed are returned as [`FailedNote`]s.
    /// 2. Execution – the remaining notes are executed against the account state.
    /// 3. Proving – a proof is generated for the executed transaction.
    /// 4. Submission – the proven transaction is submitted to the block producer.
    ///
    /// # Returns
    ///
    /// On success, returns the list of [`FailedNote`]s representing notes that were
    /// filtered out before execution.
    ///
    /// # Errors
    ///
    /// Returns an [`NtxError`] if any step of the pipeline fails, including:
    /// - Note filtering (e.g., all notes fail consumability checks).
    /// - Transaction execution.
    /// - Proof generation.
    /// - Submission to the network.
    #[instrument(target = COMPONENT, name = "ntx.execute_transaction", skip_all, err)]
    pub fn execute_transaction(
        self,
        tx: TransactionCandidate,
    ) -> impl FutureMaybeSend<NtxResult<Vec<FailedNote>>> {
        let TransactionCandidate {
            account,
            notes,
            chain_tip_header,
            chain_mmr,
        } = tx;
        tracing::Span::current().set_attribute("account.id", account.id());
        tracing::Span::current()
            .set_attribute("account.id.network_prefix", account.id().prefix().to_string().as_str());
        tracing::Span::current().set_attribute("notes.count", notes.len());
        tracing::Span::current()
            .set_attribute("reference_block.number", chain_tip_header.block_num());

        async move {
            async move {
                let data_store = NtxDataStore::new(account, chain_tip_header, chain_mmr);

                let notes = notes.into_iter().map(Note::from).collect::<Vec<_>>();
                let (successful, failed) = self.filter_notes(&data_store, notes).await?;
                let executed = Box::pin(self.execute(&data_store, successful)).await?;
                let proven = Box::pin(self.prove(executed)).await?;
                self.submit(proven).await?;
                Ok(failed)
            }
            .in_current_span()
            .await
            .inspect_err(|err| tracing::Span::current().set_error(err))
        }
    }

    /// Filters a collection of notes, returning only those that can be successfully executed
    /// against the given network account.
    ///
    /// This function performs a consumability check on each provided note and partitions them into
    /// two sets:
    /// - Successful notes: notes that can be executed and are returned wrapped in [`InputNotes`].
    /// - Failed notes: notes that cannot be executed.
    ///
    /// # Guarantees
    ///
    /// - On success, the returned [`InputNotes`] set is guaranteed to be non-empty.
    /// - The original ordering of notes is not preserved if any notes have failed.
    ///
    /// # Errors
    ///
    /// Returns an [`NtxError`] if:
    /// - The consumability check fails unexpectedly.
    /// - All notes fail the check (i.e., no note is consumable).
    #[instrument(target = COMPONENT, name = "ntx.execute_transaction.filter_notes", skip_all, err)]
    async fn filter_notes(
        &self,
        data_store: &NtxDataStore,
        notes: Vec<Note>,
    ) -> NtxResult<(InputNotes<InputNote>, Vec<FailedNote>)> {
        let executor: TransactionExecutor<'_, '_, _, UnreachableAuth> =
            TransactionExecutor::new(data_store);
        let checker = NoteConsumptionChecker::new(&executor);

        let notes = InputNotes::from_unauthenticated_notes(notes).map_err(NtxError::InputNotes)?;

        match Box::pin(checker.check_notes_consumability(
            data_store.account.id(),
            data_store.reference_header.block_num(),
            notes.clone(),
            TransactionArgs::default(),
        ))
        .await
        {
            Ok(NoteConsumptionInfo { successful, failed, .. }) => {
                // Map successful notes to input notes.
                let successful = InputNotes::from_unauthenticated_notes(successful)
                    .map_err(NtxError::InputNotes)?;

                // If none are successful, abort.
                if successful.is_empty() {
                    return Err(NtxError::AllNotesFailed(failed));
                }

                Ok((successful, failed))
            },
            Err(err) => return Err(NtxError::NoteFilter(err)),
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
            TransactionExecutor::new(data_store);

        Box::pin(executor.execute_transaction(
            data_store.account.id(),
            data_store.reference_header.block_num(),
            notes,
            TransactionArgs::default(),
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
