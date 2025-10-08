# miden-node-grpc-error-macro

Procedural macro for deriving the `GrpcError` trait on error enums, reducing boilerplate in gRPC error handling.

## Overview

This macro automatically generates:
1. A companion `*GrpcError` enum with `#[repr(u8)]` for wire serialization
2. An `api_error()` method that maps error variants to their gRPC representations
3. Implementation of the `GrpcError` trait for the companion enum

## Usage

### Basic Example

```rust
use miden_node_grpc_error_macro::GrpcError;
use thiserror::Error;

#[derive(Debug, Error, GrpcError)]
pub enum GetNoteScriptByRootError {
    #[error("database error")]
    #[grpc(internal)]
    DatabaseError(#[from] DatabaseError),
    
    #[error("malformed script root")]
    DeserializationFailed(#[from] ConversionError),
    
    #[error("script with given root doesn't exist")]
    ScriptNotFound,
}
```

### Generated Code

The above example generates:

```rust
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum GetNoteScriptByRootGrpcError {
    /// Internal server error
    Internal = 0,
    /// Malformed script root
    DeserializationFailed = 1,
    /// Script with given root doesn't exist
    ScriptNotFound = 2,
}

impl GrpcError for GetNoteScriptByRootGrpcError {
    fn api_code(self) -> u8 {
        self as u8
    }
    
    fn is_internal(&self) -> bool {
        matches!(self, Self::Internal)
    }
}

impl GetNoteScriptByRootError {
    pub fn api_error(&self) -> GetNoteScriptByRootGrpcError {
        match self {
            GetNoteScriptByRootError::DatabaseError(_) => 
                GetNoteScriptByRootGrpcError::Internal,
            GetNoteScriptByRootError::DeserializationFailed(_) => 
                GetNoteScriptByRootGrpcError::DeserializationFailed,
            GetNoteScriptByRootError::ScriptNotFound => 
                GetNoteScriptByRootGrpcError::ScriptNotFound,
        }
    }
}

impl From<GetNoteScriptByRootError> for tonic::Status {
    fn from(value: GetNoteScriptByRootError) -> Self {
        let api_error = value.api_error();
        let message = if api_error.is_internal() {
            "Internal error".to_owned()
        } else {
            value.as_report()
        };
        tonic::Status::with_details(
            api_error.tonic_code(),
            message,
            vec![api_error.api_code()].into(),
        )
    }
}
```

## Attributes

### `#[grpc(internal)]`

Mark a variant as an internal error. This will:
- Map the variant to `*GrpcError::Internal` instead of creating a separate variant
- Result in the error being serialized with `tonic::Code::Internal` instead of `tonic::Code::InvalidArgument`

```rust
#[derive(Debug, Error, GrpcError)]
pub enum MyError {
    #[error("database connection failed")]
    #[grpc(internal)]  // Maps to MyGrpcError::Internal
    DatabaseError(#[from] DatabaseError),
    
    #[error("invalid input")]
    InvalidInput,  // Creates MyGrpcError::InvalidInput variant
}
```

