// SYNC ENDPOINTS
// ================================================================================================

// Common trait for all gRPC error enums
pub trait GrpcError {
    fn api_code(self) -> u8;
    fn is_internal(&self) -> bool;

    fn tonic_code(&self) -> tonic::Code {
        if self.is_internal() {
            tonic::Code::Internal
        } else {
            tonic::Code::InvalidArgument
        }
    }
}

// SYNC NULLIFIERS
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Copy, Clone)]
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

impl GrpcError for SyncNullifiersGrpcError {
    fn api_code(self) -> u8 {
        self as u8
    }

    fn is_internal(&self) -> bool {
        matches!(self, SyncNullifiersGrpcError::Internal)
    }
}

// SYNC ACCOUNT VAULT
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Copy, Clone)]
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

impl GrpcError for SyncAccountVaultGrpcError {
    fn api_code(self) -> u8 {
        self as u8
    }

    fn is_internal(&self) -> bool {
        matches!(self, SyncAccountVaultGrpcError::Internal)
    }
}

// SYNC NOTES
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Copy, Clone)]
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

impl GrpcError for SyncNotesGrpcError {
    fn api_code(self) -> u8 {
        self as u8
    }

    fn is_internal(&self) -> bool {
        matches!(self, SyncNotesGrpcError::Internal)
    }
}

// SYNC STORAGE MAPS
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Copy, Clone)]
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

impl GrpcError for SyncStorageMapsGrpcError {
    fn api_code(self) -> u8 {
        self as u8
    }

    fn is_internal(&self) -> bool {
        matches!(self, SyncStorageMapsGrpcError::Internal)
    }
}

// GET ENDPOINTS
// ================================================================================================

// GET BLOCK BY NUMBER
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Copy, Clone)]
#[repr(u8)]
pub enum GetBlockByNumberGrpcError {
    // Internal Server Error
    Internal = 0,
    // Malformed block number
    DeserializationFailed = 1,
}

impl GrpcError for GetBlockByNumberGrpcError {
    fn api_code(self) -> u8 {
        self as u8
    }

    fn is_internal(&self) -> bool {
        matches!(self, GetBlockByNumberGrpcError::Internal)
    }
}

// GET BLOCK HEADER BY NUMBER
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Copy, Clone)]
#[repr(u8)]
pub enum GetBlockHeaderByNumberGrpcError {
    // Internal Server Error
    Internal = 0,
    // Malformed block number
    DeserializationFailed = 1,
}

impl GrpcError for GetBlockHeaderByNumberGrpcError {
    fn api_code(self) -> u8 {
        self as u8
    }

    fn is_internal(&self) -> bool {
        matches!(self, GetBlockHeaderByNumberGrpcError::Internal)
    }
}

// GET NOTES BY ID
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Copy, Clone)]
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

impl GrpcError for GetNotesByIdGrpcError {
    fn api_code(self) -> u8 {
        self as u8
    }

    fn is_internal(&self) -> bool {
        matches!(self, GetNotesByIdGrpcError::Internal)
    }
}

// GET NOTE SCRIPT BY ROOT
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Copy, Clone)]
#[repr(u8)]
pub enum GetNoteScriptByRootGrpcError {
    // Internal Server Error
    Internal = 0,
    // Malformed script root
    DeserializationFailed = 1,
    // Script with given root doesn't exist
    ScriptNotFound = 2,
}

impl GrpcError for GetNoteScriptByRootGrpcError {
    fn api_code(self) -> u8 {
        self as u8
    }

    fn is_internal(&self) -> bool {
        matches!(self, GetNoteScriptByRootGrpcError::Internal)
    }
}

// CHECK NULLIFIERS
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Copy, Clone)]
#[repr(u8)]
pub enum CheckNullifiersGrpcError {
    // Internal Server Error
    Internal = 0,
    // Malformed nullifier
    DeserializationFailed = 1,
    // Too many nullifiers in request
    TooManyNullifiers = 2,
}

impl GrpcError for CheckNullifiersGrpcError {
    fn api_code(self) -> u8 {
        self as u8
    }

    fn is_internal(&self) -> bool {
        matches!(self, CheckNullifiersGrpcError::Internal)
    }
}
