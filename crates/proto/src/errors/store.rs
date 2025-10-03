use crate::errors::GrpcError;
use crate::grpc_error;

// SYNC ENDPOINTS
// ================================================================================================

// SYNC NULLIFIERS
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum SyncNullifiersGrpcError {
    // Internal Server Error
    Internal = 0,
    // Malformed nullifier prefix
    DeserializationFailed = 1,
    // Invalid `block_from`/`block_to` parameters
    InvalidBlockRange = 2,
    // Unsupported prefix length (only `16` supported)
    InvalidPrefixLength = 3,
}

grpc_error!(SyncNullifiersGrpcError);

// SYNC ACCOUNT VAULT
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum SyncAccountVaultGrpcError {
    // Internal Server Error
    Internal = 0,
    // Malformed account ID
    DeserializationFailed = 1,
    // Invalid `block_from`/`block_to` parameters
    InvalidBlockRange = 2,
    // Account is not public (no vault sync)
    AccountNotPublic = 3,
}

grpc_error!(SyncAccountVaultGrpcError);

// SYNC NOTES
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum SyncNotesGrpcError {
    // Internal Server Error
    Internal = 0,
    // Malformed note tags
    DeserializationFailed = 1,
    // Invalid `block_from`/`block_to` parameters
    InvalidBlockRange = 2,
    // Too many note tags in request
    TooManyTags = 3,
}

grpc_error!(SyncNotesGrpcError);

// SYNC STORAGE MAPS
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum SyncStorageMapsGrpcError {
    // Internal Server Error
    Internal = 0,
    // Malformed account ID
    DeserializationFailed = 1,
    // Invalid `block_from`/`block_to` parameters
    InvalidBlockRange = 2,
    // Account ID does not exist
    AccountNotFound = 3,
    // Account storage is not publicly accessible
    AccountNotPublic = 4,
}

grpc_error!(SyncStorageMapsGrpcError);

// SYNC TRANSACTIONS
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum SyncTransactionsGrpcError {
    // Internal Server Error
    Internal = 0,
    // Malformed account ID
    DeserializationFailed = 1,
    // Invalid `block_from`/`block_to` parameters
    InvalidBlockRange = 2,
    // Account ID does not exist
    AccountNotFound = 3,
    // Too many account IDs in request
    TooManyAccountIds = 4,
}

grpc_error!(SyncTransactionsGrpcError);

// GET ENDPOINTS
// ================================================================================================

// GET BLOCK BY NUMBER
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum GetBlockByNumberGrpcError {
    // Internal Server Error
    Internal = 0,
    // Malformed block number
    DeserializationFailed = 1,
}

grpc_error!(GetBlockByNumberGrpcError);

// GET BLOCK HEADER BY NUMBER
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum GetBlockHeaderByNumberGrpcError {
    // Internal Server Error
    Internal = 0,
    // Malformed block number
    DeserializationFailed = 1,
}

grpc_error!(GetBlockHeaderByNumberGrpcError);

// GET NOTES BY ID
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum GetNotesByIdGrpcError {
    // Internal Server Error
    Internal = 0,
    // Malformed note ID
    DeserializationFailed = 1,
    // One or more note IDs don't exist
    NoteNotFound = 2,
    // Too many note IDs in request
    TooManyNoteIds = 3,
    // Note details not publicly accessible
    NoteNotPublic = 4,
}

grpc_error!(GetNotesByIdGrpcError);

// GET NOTE SCRIPT BY ROOT
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum GetNoteScriptByRootGrpcError {
    // Internal Server Error
    Internal = 0,
    // Malformed script root
    DeserializationFailed = 1,
    // Script with given root doesn't exist
    ScriptNotFound = 2,
}

grpc_error!(GetNoteScriptByRootGrpcError);

// CHECK NULLIFIERS
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum CheckNullifiersGrpcError {
    // Internal Server Error
    Internal = 0,
    // Malformed nullifier
    DeserializationFailed = 1,
    // Too many nullifiers in request
    TooManyNullifiers = 2,
}

grpc_error!(CheckNullifiersGrpcError);
