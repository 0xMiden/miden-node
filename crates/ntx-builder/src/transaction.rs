use std::{collections::BTreeSet, sync::Arc};

use futures::TryFutureExt;
use miden_node_utils::{ErrorReport, tracing::OpenTelemetrySpanExt};
use miden_objects::{
    TransactionInputError, Word,
    account::{Account, AccountId},
    assembly::DefaultSourceManager,
    block::{BlockHeader, BlockNumber},
    note::{Note, NoteId},
    transaction::{
        ExecutedTransaction, InputNote, InputNotes, PartialBlockchain, ProvenTransaction,
        TransactionArgs,
    },
};
use miden_remote_prover_client::remote_prover::tx_prover::RemoteTransactionProver;
use miden_tx::{
    DataStore, DataStoreError, LocalTransactionProver, MastForestStore, NoteAccountExecution,
    NoteConsumptionChecker, TransactionExecutor, TransactionExecutorError, TransactionMastStore,
    TransactionProverError, auth::UnreachableAuth,
};
use tokio::task::JoinError;
use tracing::{Instrument, instrument, instrument::Instrumented};

use crate::{COMPONENT, block_producer::BlockProducerClient, state::TransactionCandidate};

#[derive(Debug, thiserror::Error)]
pub enum NtxError {
    #[error("note inputs were invalid")]
    InputNotes(#[source] TransactionInputError),
    #[error("failed to filter notes")]
    NoteFilter(#[source] TransactionExecutorError),
    #[error("no viable notes: {0}")]
    NoViableNotes(String),
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
    pub async fn execute_transaction(self, tx: TransactionCandidate) -> NtxResult<Option<NoteId>> {
        let TransactionCandidate {
            account,
            account_id_prefix,
            notes,
            chain_tip_header,
            chain_mmr,
        } = tx;

        tracing::Span::current()
            .set_attribute("account.id_prefix", account_id_prefix.to_string().as_str());
        tracing::Span::current().set_attribute("notes.count", notes.len());
        tracing::Span::current()
            .set_attribute("reference_block.number", chain_tip_header.block_num());

        // Work-around for `TransactionExecutor` not being `Send`.
        tokio::task::spawn_blocking(move || {
            {
                {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("runtime should be built");

                    rt.block_on(
                        async move {
                            let notes = notes
                                .into_iter()
                                .map(|note| note.into_inner().into())
                                .collect::<Vec<Note>>();
                            let notes = InputNotes::from_unauthenticated_notes(notes)
                                .map_err(NtxError::InputNotes)?;

                            let data_store =
                                NtxDataStore::new(account, chain_tip_header, chain_mmr);

                            let (notes, failed_note_id) =
                                self.filter_notes(&data_store, notes).await?;
                            self.execute(&data_store, notes)
                                .and_then(|tx| self.prove(tx))
                                .and_then(|tx| self.submit(tx))
                                .await?;
                            Ok(failed_note_id)
                        }
                        .in_current_span(),
                    )
                }
            }
            .in_current_span()
        })
        .await
        .map_err(NtxError::Panic)
        .and_then(Instrumented::into_inner)
        .inspect_err(|err| tracing::Span::current().set_error(err))
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
    ) -> NtxResult<(InputNotes<InputNote>, Option<NoteId>)> {
        let executor: TransactionExecutor<'_, '_, _, UnreachableAuth> =
            TransactionExecutor::new(data_store, None);
        let checker = NoteConsumptionChecker::new(&executor);

        match checker
            .check_notes_consumability(
                data_store.account.id(),
                data_store.reference_header.block_num(),
                notes.clone(),
                TransactionArgs::default(),
                Arc::new(DefaultSourceManager::default()),
            )
            .await
        {
            Ok(NoteAccountExecution::Success) => Ok((notes, None)),
            Ok(NoteAccountExecution::Failure {
                successful_notes,
                error,
                failed_note_id, // kept in case you need it later
                ..
            }) => {
                // Gather successful notes.
                let successful_notes = successful_notes
                    .into_iter()
                    .filter_map(|id| notes.iter().find(|note| note.id() == id))
                    .cloned()
                    .collect::<Vec<_>>();

                // If none are successful, abort.
                if successful_notes.is_empty() {
                    tracing::warn!(
                        err = %error.as_ref().map_or_else(|| "none".to_string(), ErrorReport::as_report),
                        "failed to check note consumability",
                    );
                    let error = error.map_or_else(|| "none".to_string(), |e| e.as_report());
                    return Err(NtxError::NoViableNotes(error));
                }

                // If there were some successful notes but still an error, log it
                let successful_count = successful_notes.len();
                if let Some(err) = error {
                    tracing::warn!(
                        %err,
                        successful_count,
                        "failed to check note consumability",
                    );
                }

                // Return successful and failed note id.
                Ok((InputNotes::new_unchecked(successful_notes), Some(failed_note_id)))
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
            TransactionExecutor::new(data_store, None);

        executor
            .execute_transaction(
                data_store.account.id(),
                data_store.reference_header.block_num(),
                notes,
                TransactionArgs::default(),
                Arc::new(DefaultSourceManager::default()),
            )
            .await
            .map_err(NtxError::Execution)
    }

    /// Delegates the transaction proof to the remote prover if configured, otherwise performs the
    /// proof locally.
    #[instrument(target = COMPONENT, name = "ntx.execute_transaction.prove", skip_all, err)]
    async fn prove(&self, tx: ExecutedTransaction) -> NtxResult<ProvenTransaction> {
        use miden_tx::TransactionProver;

        if let Some(remote) = &self.prover {
            remote.prove(tx.into()).await
        } else {
            LocalTransactionProver::default().prove(tx.into()).await
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

#[async_trait::async_trait(?Send)]
impl DataStore for NtxDataStore {
    async fn get_transaction_inputs(
        &self,
        account_id: AccountId,
        ref_blocks: BTreeSet<BlockNumber>,
    ) -> Result<(Account, Option<Word>, BlockHeader, PartialBlockchain), DataStoreError> {
        if self.account.id() != account_id {
            return Err(DataStoreError::AccountNotFound(account_id));
        }

        match ref_blocks.last().copied() {
            Some(reference) if reference == self.reference_header.block_num() => {},
            Some(other) => return Err(DataStoreError::BlockNotFound(other)),
            None => return Err(DataStoreError::other("no reference block requested")),
        }

        Ok((
            self.account.clone(),
            None,
            self.reference_header.clone(),
            self.chain_mmr.clone(),
        ))
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
