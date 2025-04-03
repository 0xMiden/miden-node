use std::io;

use deadpool::managed::PoolError;
use miden_objects::{
    AccountDeltaError, AccountError, NoteError,
    account::AccountId,
    block::BlockNumber,
    crypto::{
        hash::rpo::RpoDigest,
        merkle::{MerkleError, MmrError},
        utils::DeserializationError,
    },
    note::Nullifier,
    transaction::OutputNote,
};
use rusqlite::types::FromSqlError;
use thiserror::Error;
use tokio::sync::oneshot::error::RecvError;
use tonic::Status;

// INTERNAL ERRORS
// =================================================================================================

#[derive(Debug, Error)]
pub enum NullifierTreeError {
    #[error("failed to create nullifier tree")]
    CreationFailed(#[source] MerkleError),

    #[error("failed to mutate nullifier tree")]
    MutationFailed(#[source] MerkleError),
}

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
    #[error("SQLite deserialization error")]
    FromSqlError(#[from] FromSqlError),
    #[error("I/O error")]
    IoError(#[from] io::Error),
    #[error("migration failed")]
    MigrationError(#[from] rusqlite_migration::Error),
    #[error("missing database connection")]
    MissingDbConnection(#[from] PoolError<rusqlite::Error>),
    #[error("note error")]
    NoteError(#[from] NoteError),
    #[error("SQLite error")]
    SqliteError(#[from] rusqlite::Error),

    // OTHER ERRORS
    // ---------------------------------------------------------------------------------------------
    #[error("account commitment mismatch (expected {expected}, but calculated is {calculated})")]
    AccountCommitmentsMismatch {
        expected: RpoDigest,
        calculated: RpoDigest,
    },
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
    FailedToCreateAccountsTree(#[from] MerkleError),
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
    #[error("SQLite migration error")]
    SqliteMigration(#[from] rusqlite_migration::Error),
}

#[derive(Debug, Error)]
pub enum GenesisError {
    // ERRORS WITH AUTOMATIC CONVERSIONS FROM NESTED ERROR TYPES
    // ---------------------------------------------------------------------------------------------
    #[error("database error")]
    Database(#[from] DatabaseError),
    // TODO: Check if needed.
    #[error("block error")]
    Block,
    #[error("merkle error")]
    Merkle(#[from] MerkleError),
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
    InvalidBlockTxCommitment { expected: RpoDigest, actual: RpoDigest },
    #[error("received invalid account tree root")]
    NewBlockInvalidAccountRoot,
    #[error("new block number must be 1 greater than the current block number")]
    NewBlockInvalidBlockNum,
    #[error("new block chain commitment is not consistent with chain MMR")]
    NewBlockInvalidChainCommitment,
    #[error("received invalid note root")]
    NewBlockInvalidNoteRoot,
    #[error("received invalid nullifier root")]
    NewBlockInvalidNullifierRoot,
    #[error("new block `prev_block_commitment` must match the chain's tip")]
    NewBlockInvalidPrevCommitment,
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

#[derive(Error, Debug)]
pub enum NoteSyncError {
    #[error("database error")]
    DatabaseError(#[from] DatabaseError),
    #[error("block headers table is empty")]
    EmptyBlockHeadersTable,
    #[error("error retrieving the merkle proof for the block")]
    MmrError(#[from] MmrError),
}

#[derive(Error, Debug)]
pub enum GetBatchInputsError {
    #[error("failed to select note inclusion proofs")]
    SelectNoteInclusionProofError(#[source] DatabaseError),
    #[error("failed to select block headers")]
    SelectBlockHeaderError(#[source] DatabaseError),
    #[error("set of blocks refernced by transactions is empty")]
    TransactionBlockReferencesEmpty,
    #[error(
        "highest block number {highest_block_num} referenced by a transaction is newer than the latest block {latest_block_num}"
    )]
    UnknownTransactionBlockReference {
        highest_block_num: BlockNumber,
        latest_block_num: BlockNumber,
    },
}
