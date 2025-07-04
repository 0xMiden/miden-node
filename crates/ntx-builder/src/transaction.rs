use std::{collections::BTreeSet, sync::Arc};

use futures::TryFutureExt;
use miden_node_proto::domain::{account::NetworkAccountPrefix, note::NetworkNote};
use miden_objects::{
    AccountError, TransactionInputError, Word,
    account::{Account, AccountId, delta::AccountUpdateDetails},
    assembly::DefaultSourceManager,
    block::{BlockHeader, BlockNumber},
    note::NoteTag,
    transaction::{
        ExecutedTransaction, InputNote, InputNotes, PartialBlockchain, ProvenTransaction,
        TransactionArgs, TxAccountUpdate,
    },
};
use miden_remote_prover_client::remote_prover::tx_prover::RemoteTransactionProver;
use miden_tx::{
    DataStore, DataStoreError, LocalTransactionProver, MastForestStore, NoteAccountExecution,
    NoteConsumptionChecker, TransactionExecutor, TransactionExecutorError, TransactionProverError,
};

use crate::builder::{
    block_producer::BlockProducerClient,
    store::{StoreClient, StoreError},
};

#[derive(Clone)]
pub struct NtxContext {
    store: StoreClient,
    block_producer: BlockProducerClient,
    genesis_header: BlockHeader,
    prover: Option<RemoteTransactionProver>,
}

#[derive(Debug, thiserror::Error)]
pub enum NtxError {
    #[error(transparent)]
    Store(StoreError),
    #[error("the network account prefix {0} has no associated account")]
    UnknownAccount(NetworkAccountPrefix),
    #[error("failed to apply account deltas")]
    ApplyDeltas(#[source] AccountError),
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
}

type NtxResult<T> = Result<T, NtxError>;

impl NtxContext {
    pub async fn execute_transaction(
        self,
        account: NetworkAccountPrefix,
        deltas: Vec<TxAccountUpdate>,
        notes: Vec<NetworkNote>,
    ) -> NtxResult<()> {
        let account = self.provision_account(account, deltas).await?;

        let notes = notes
            .into_iter()
            .map(|note| InputNote::Unauthenticated { note: note.into() })
            .collect();
        let notes = InputNotes::new(notes).map_err(NtxError::InputNotes)?;
        let data_store = NtxDataStore {
            account,
            genesis_header: self.genesis_header.clone(),
        };

        self.filter_notes(&data_store, notes)
            .and_then(|notes| self.execute(&data_store, notes))
            .and_then(|tx| self.prove(tx))
            .and_then(|tx| self.submit(tx))
            .await
    }

    async fn provision_account(
        &self,
        account: NetworkAccountPrefix,
        mut deltas: Vec<TxAccountUpdate>,
    ) -> NtxResult<Account> {
        // The account might be created as part of the deltas in which case we don't need to hit the store.
        //
        // Its a bit difficult to both match and return the inner value, so this feels awkward.
        let mut account = match deltas.first().map(TxAccountUpdate::details) {
            Some(AccountUpdateDetails::New(account)) => {
                let account = account.clone();
                deltas.pop();
                account
            },
            _ => {
                let note_tag = NoteTag::NetworkAccount(account.inner());
                self.store
                    .get_network_account_by_tag(note_tag)
                    .await
                    .map_err(NtxError::Store)?
                    .ok_or(NtxError::UnknownAccount(account))?
            },
        };
        let init_commitment = account.commitment();

        // Its possible for deltas to already be committed at the store.
        let deltas = deltas
            .into_iter()
            .skip_while(|delta| delta.initial_state_commitment() != init_commitment);

        for delta in deltas {
            match delta.details() {
                AccountUpdateDetails::Delta(delta) => {
                    account.apply_delta(delta).map_err(NtxError::ApplyDeltas)?
                },
                _ => unreachable!("Handle this error"),
            };
        }

        Ok(account)
    }

    async fn filter_notes(
        &self,
        data_store: &NtxDataStore,
        notes: InputNotes<InputNote>,
    ) -> NtxResult<InputNotes<InputNote>> {
        let executor = TransactionExecutor::new(data_store, None);
        let checker = NoteConsumptionChecker::new(&executor);

        let notes = match checker
            .check_notes_consumability(
                data_store.account.id(),
                BlockNumber::GENESIS,
                notes.clone(),
                TransactionArgs::default(),
                Arc::new(DefaultSourceManager::default()),
            )
            .await
        {
            Ok(NoteAccountExecution::Success) => notes,
            Ok(NoteAccountExecution::Failure { successful_notes, .. }) => {
                let notes = successful_notes
                    .into_iter()
                    .map(|id| notes.iter().find(|note| note.id() == id).unwrap())
                    .cloned()
                    .collect();

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

    async fn execute(
        &self,
        data_store: &NtxDataStore,
        notes: InputNotes<InputNote>,
    ) -> NtxResult<ExecutedTransaction> {
        let executor = TransactionExecutor::new(data_store, None);

        executor
            .execute_transaction(
                data_store.account.id(),
                BlockNumber::GENESIS,
                notes,
                TransactionArgs::default(),
                Arc::new(DefaultSourceManager::default()),
            )
            .await
            .map_err(NtxError::Execution)
    }

    async fn prove(&self, tx: ExecutedTransaction) -> NtxResult<ProvenTransaction> {
        use miden_tx::TransactionProver;

        if let Some(remote) = &self.prover {
            remote.prove(tx.into()).await
        } else {
            LocalTransactionProver::default().prove(tx.into()).await
        }
        .map_err(NtxError::Proving)
    }

    async fn submit(&self, tx: ProvenTransaction) -> NtxResult<()> {
        self.block_producer
            .submit_proven_transaction(tx)
            .await
            .map_err(NtxError::Submission)
    }
}

struct NtxDataStore {
    account: Account,
    genesis_header: BlockHeader,
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
            Some(BlockNumber::GENESIS) => {},
            Some(other) => return Err(DataStoreError::BlockNotFound(other)),
            // TODO: is this fine to do?
            None => return Err(DataStoreError::other("no reference block requested")),
        }

        Ok((
            self.account.clone(),
            None,
            self.genesis_header.clone(),
            // TODO: is this correct or should one actually construct it from the genesis header
            PartialBlockchain::default(),
        ))
    }
}

impl MastForestStore for NtxDataStore {
    fn get(&self, _: &miden_objects::Digest) -> Option<std::sync::Arc<miden_objects::MastForest>> {
        None
    }
}
