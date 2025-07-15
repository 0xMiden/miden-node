use std::io;

use deadpool_sync::InteractError;
use miden_node_proto::domain::account::NetworkAccountError;
use miden_node_utils::limiter::QueryLimitError;
use miden_objects::{
    AccountDeltaError, AccountError, AccountTreeError, NoteError, NullifierTreeError, Word,
    account::AccountId,
    block::BlockNumber,
    crypto::{merkle::MmrError, utils::DeserializationError},
    note::Nullifier,
    transaction::OutputNote,
};
use thiserror::Error;
use tokio::sync::oneshot::error::RecvError;
use tonic::Status;

// DATABASE ERRORS
// =================================================================================================

#[derive(Debug, Error)]
pub enum DatabaseError {
    // ERRORS WITH AUTOMATIC CONVERSIONS FROM NESTED ERROR TYPES
    // ---------------------------------------------------------------------------------------------
    #[error("account error")]
    AccountError(#[from] AccountError),
    #[error("account delta error")]
    AccountDeltaError(#[from] AccountDeltaError),
    #[error("closed channel")]
    ClosedChannel(#[from] RecvError),
    #[error("deserialization failed")]
    DeserializationError(#[from] DeserializationError),
    #[error("hex parsing error")]
    FromHexError(#[from] hex::FromHexError),
    #[error("I/O error")]
    IoError(#[from] io::Error),
    #[error("merkle error")]
    MerkleError(#[from] miden_objects::crypto::merkle::MerkleError),
    #[error("network account error")]
    NetworkAccountError(#[from] NetworkAccountError),
    #[error("note error")]
    NoteError(#[from] NoteError),
    #[error("Setup deadpool connection pool failed")]
    Deadpool(#[from] deadpool::managed::PoolError<deadpool_diesel::Error>),
    #[error("Setup deadpool connection pool failed")]
    ConnectionPoolObtainError(#[from] Box<dyn std::error::Error + Send + Sync + 'static>),
    #[error(transparent)]
    Diesel(#[from] diesel::result::Error),
    #[error("Sqlite FFI boundary NUL termination error (not much you can do, file an issue)")]
    DieselSqliteFfi(#[from] std::ffi::NulError),
    #[error(transparent)]
    DeadpoolDiesel(#[from] deadpool_diesel::Error),
    #[error(transparent)]
    PoolRecycle(#[from] deadpool::managed::RecycleError<deadpool_diesel::Error>),
    #[error(transparent)]
    QueryParamLimit(#[from] QueryLimitError),

    // OTHER ERRORS
    // ---------------------------------------------------------------------------------------------
    #[error("account commitment mismatch (expected {expected}, but calculated is {calculated})")]
    AccountCommitmentsMismatch { expected: Word, calculated: Word },
    #[error("account {0} not found")]
    AccountNotFoundInDb(AccountId),
    #[error("accounts {0:?} not found")]
    AccountsNotFoundInDb(Vec<AccountId>),
    #[error("account {0} is not on the chain")]
    AccountNotPublic(AccountId),
    #[error("data corrupted: {0}")]
    DataCorrupted(String),
    #[error("SQLite pool interaction failed: {0}")]
    InteractError(String),
    #[error("invalid Felt: {0}")]
    InvalidFelt(String),
    #[error(
        "unsupported database version. There is no migration chain from/to this version. \
        Remove all database files and try again."
    )]
    UnsupportedDatabaseVersion,
}

impl DatabaseError {
    /// Converts from `InteractError`
    ///
    /// Note: Required since `InteractError` has at least one enum
    /// variant that is _not_ `Send + Sync` and hence prevents the
    /// `Sync` auto implementation.
    /// This does an internal conversion to string while maintaining
    /// convenience.
    ///
    /// Using `MSG` as const so it can be called as
    /// `.map_err(DatabaseError::interact::<"Your message">)`
    pub fn interact(msg: &(impl ToString + ?Sized), e: &InteractError) -> Self {
        let msg = msg.to_string();
        Self::InteractError(format!("{msg} failed: {e:?}"))
    }
}

impl From<DatabaseError> for Status {
    fn from(err: DatabaseError) -> Self {
        match err {
            DatabaseError::AccountNotFoundInDb(_)
            | DatabaseError::AccountsNotFoundInDb(_)
            | DatabaseError::AccountNotPublic(_) => Status::not_found(err.to_string()),

            _ => Status::internal(err.to_string()),
        }
    }
}

// INITIALIZATION ERRORS
// =================================================================================================

#[derive(Error, Debug)]
pub enum StateInitializationError {
    #[error("database error")]
    DatabaseError(#[from] DatabaseError),
    #[error("failed to create nullifier tree")]
    FailedToCreateNullifierTree(#[from] NullifierTreeError),
    #[error("failed to create accounts tree")]
    FailedToCreateAccountsTree(#[source] AccountTreeError),
}

#[derive(Debug, Error)]
pub enum DatabaseSetupError {
    #[error("I/O error")]
    Io(#[from] io::Error),
    #[error("database error")]
    Database(#[from] DatabaseError),
    #[error("genesis block error")]
    GenesisBlock(#[from] GenesisError),
    #[error("pool build error")]
    PoolBuild(#[from] deadpool::managed::BuildError),
    #[error("Setup deadpool connection pool failed")]
    Pool(#[from] deadpool::managed::PoolError<deadpool_diesel::Error>),
}

#[derive(Debug, Error)]
pub enum GenesisError {
    // ERRORS WITH AUTOMATIC CONVERSIONS FROM NESTED ERROR TYPES
    // ---------------------------------------------------------------------------------------------
    #[error("database error")]
    Database(#[from] DatabaseError),
    #[error("failed to build genesis account tree")]
    AccountTree(#[source] AccountTreeError),
    #[error("failed to deserialize genesis file")]
    GenesisFileDeserialization(#[from] DeserializationError),
}

// ENDPOINT ERRORS
// =================================================================================================
#[derive(Error, Debug)]
pub enum InvalidBlockError {
    #[error("duplicated nullifiers {0:?}")]
    DuplicatedNullifiers(Vec<Nullifier>),
    #[error("invalid output note type: {0:?}")]
    InvalidOutputNoteType(Box<OutputNote>),
    #[error("invalid block tx commitment: expected {expected}, but got {actual}")]
    InvalidBlockTxCommitment { expected: Word, actual: Word },
    #[error("received invalid account tree root")]
    NewBlockInvalidAccountRoot,
    #[error("new block number must be 1 greater than the current block number")]
    NewBlockInvalidBlockNum {
        expected: BlockNumber,
        submitted: BlockNumber,
    },
    #[error("new block chain commitment is not consistent with chain MMR")]
    NewBlockInvalidChainCommitment,
    #[error("received invalid note root")]
    NewBlockInvalidNoteRoot,
    #[error("received invalid nullifier root")]
    NewBlockInvalidNullifierRoot,
    #[error("new block `prev_block_commitment` must match the chain's tip")]
    NewBlockInvalidPrevCommitment,
    #[error("nullifier in new block is already spent")]
    NewBlockNullifierAlreadySpent(#[source] NullifierTreeError),
    #[error("duplicate account ID prefix in new block")]
    NewBlockDuplicateAccountIdPrefix(#[source] AccountTreeError),
}

#[derive(Error, Debug)]
pub enum ApplyBlockError {
    // ERRORS WITH AUTOMATIC CONVERSIONS FROM NESTED ERROR TYPES
    // ---------------------------------------------------------------------------------------------
    #[error("database error")]
    DatabaseError(#[from] DatabaseError),
    #[error("I/O error")]
    IoError(#[from] io::Error),
    #[error("task join error")]
    TokioJoinError(#[from] tokio::task::JoinError),
    #[error("invalid block error")]
    InvalidBlockError(#[from] InvalidBlockError),

    // OTHER ERRORS
    // ---------------------------------------------------------------------------------------------
    #[error("block applying was cancelled because of closed channel on database side")]
    ClosedChannel(#[from] RecvError),
    #[error("concurrent write detected")]
    ConcurrentWrite,
    #[error("database doesn't have any block header data")]
    DbBlockHeaderEmpty,
    #[error("database update failed: {0}")]
    DbUpdateTaskFailed(String),
}

impl From<ApplyBlockError> for Status {
    fn from(err: ApplyBlockError) -> Self {
        match err {
            ApplyBlockError::InvalidBlockError(_) => Status::invalid_argument(err.to_string()),

            _ => Status::internal(err.to_string()),
        }
    }
}

#[derive(Error, Debug)]
pub enum GetBlockHeaderError {
    #[error("database error")]
    DatabaseError(#[from] DatabaseError),
    #[error("error retrieving the merkle proof for the block")]
    MmrError(#[from] MmrError),
}

#[derive(Error, Debug)]
pub enum GetBlockInputsError {
    #[error("failed to select note inclusion proofs")]
    SelectNoteInclusionProofError(#[source] DatabaseError),
    #[error("failed to select block headers")]
    SelectBlockHeaderError(#[source] DatabaseError),
    #[error(
        "highest block number {highest_block_number} referenced by a batch is newer than the latest block {latest_block_number}"
    )]
    UnknownBatchBlockReference {
        highest_block_number: BlockNumber,
        latest_block_number: BlockNumber,
    },
}

#[derive(Error, Debug)]
pub enum StateSyncError {
    #[error("database error")]
    DatabaseError(#[from] DatabaseError),
    #[error("block headers table is empty")]
    EmptyBlockHeadersTable,
    #[error("failed to build MMR delta")]
    FailedToBuildMmrDelta(#[from] MmrError),
}

impl From<diesel::result::Error> for StateSyncError {
    fn from(value: diesel::result::Error) -> Self {
        Self::DatabaseError(DatabaseError::from(value))
    }
}

#[derive(Error, Debug)]
pub enum NoteSyncError {
    #[error("database error")]
    DatabaseError(#[from] DatabaseError),
    #[error("block headers table is empty")]
    EmptyBlockHeadersTable,
    #[error("error retrieving the merkle proof for the block")]
    MmrError(#[from] MmrError),
}

impl From<diesel::result::Error> for NoteSyncError {
    fn from(value: diesel::result::Error) -> Self {
        Self::DatabaseError(DatabaseError::from(value))
    }
}

#[derive(Error, Debug)]
pub enum GetCurrentBlockchainDataError {
    #[error("failed to retrieve block header")]
    ErrorRetrievingBlockHeader(#[source] DatabaseError),
    #[error("failed to instantiate MMR peaks")]
    InvalidPeaks(MmrError),
}

#[derive(Error, Debug)]
pub enum GetBatchInputsError {
    #[error("failed to select note inclusion proofs")]
    SelectNoteInclusionProofError(#[source] DatabaseError),
    #[error("failed to select block headers")]
    SelectBlockHeaderError(#[source] DatabaseError),
    #[error("set of blocks referenced by transactions is empty")]
    TransactionBlockReferencesEmpty,
    #[error(
        "highest block number {highest_block_num} referenced by a transaction is newer than the latest block {latest_block_num}"
    )]
    UnknownTransactionBlockReference {
        highest_block_num: BlockNumber,
        latest_block_num: BlockNumber,
    },
}

mod compile_tests {
    use std::marker::PhantomData;

    use super::{
        AccountDeltaError, AccountError, DatabaseError, DatabaseSetupError, DeserializationError,
        GenesisError, NetworkAccountError, NoteError, RecvError, StateInitializationError,
    };

    #[allow(dead_code)]
    fn ensure_is_error<E>(_phony: PhantomData<E>)
    where
        E: std::error::Error + Send + Sync + 'static,
    {
    }

    /// Ensure all enum variants remain compat with the desired
    /// trait bounds. Otherwise one gets very unwieldy errors.
    #[allow(dead_code)]
    fn assumed_trait_bounds_upheld() {
        ensure_is_error::<AccountError>(PhantomData);
        ensure_is_error::<AccountDeltaError>(PhantomData);
        ensure_is_error::<RecvError>(PhantomData);
        ensure_is_error::<DeserializationError>(PhantomData);
        ensure_is_error::<NetworkAccountError>(PhantomData);
        ensure_is_error::<NoteError>(PhantomData);
        ensure_is_error::<hex::FromHexError>(PhantomData);
        ensure_is_error::<deadpool::managed::PoolError<deadpool_diesel::Error>>(PhantomData);
        ensure_is_error::<diesel::result::Error>(PhantomData);
        ensure_is_error::<deadpool_diesel::Error>(PhantomData);
        ensure_is_error::<deadpool::managed::RecycleError<deadpool_diesel::Error>>(PhantomData);

        ensure_is_error::<DatabaseError>(PhantomData);
        ensure_is_error::<DatabaseSetupError>(PhantomData);
        ensure_is_error::<diesel::result::Error>(PhantomData);
        ensure_is_error::<GenesisError>(PhantomData);
        ensure_is_error::<StateInitializationError>(PhantomData);
        ensure_is_error::<deadpool::managed::PoolError<deadpool_diesel::Error>>(PhantomData);
    }
}
