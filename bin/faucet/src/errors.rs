use std::fmt::Debug;

use axum::{
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use miden_objects::{AccountError, NoteError, TransactionScriptError};
use miden_tx::{TransactionExecutorError, TransactionProverError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MintError {
    Execution(#[from] ExecutionError),
    Account(AccountError),
    Proving(TransactionProverError),
    Submission(tonic::Status),
}

pub type MintResult<T> = Result<T, MintError>;

#[derive(Debug, Error)]
pub enum ExecutionError {
    NoteCreation(NoteError),
    TransactionCompilation(TransactionScriptError),
    Execution(TransactionExecutorError),
}
