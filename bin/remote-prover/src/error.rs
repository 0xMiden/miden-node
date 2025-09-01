//! Error types for remote prover client functionality

#[cfg(not(feature = "std"))]
use alloc::boxed::Box;
#[cfg(not(feature = "std"))]
use alloc::string::{String, ToString};
#[cfg(not(feature = "std"))]
use core::error::Error as CoreError;
#[cfg(feature = "std")]
use std::error::Error as CoreError;

use thiserror::Error;

/// Remote Prover Client Error Types
/// ===============================================================================================

#[derive(Debug, Error)]
pub enum RemoteProverClientError {
    /// Indicates that the provided gRPC server endpoint is invalid.
    #[error("invalid uri {0}")]
    InvalidEndpoint(String),
    #[error("failed to connect to prover {0}")]
    /// Indicates that the connection to the server failed.
    ConnectionFailed(#[source] Box<dyn CoreError + Send + Sync + 'static>),
    /// Custom error variant for errors not covered by the other variants.
    #[error("{error_msg}")]
    Other {
        error_msg: Box<str>,
        // thiserror will return this when calling `Error::source` on
        // `RemoteProverClientError`.
        source: Option<Box<dyn CoreError + Send + Sync + 'static>>,
    },
}

impl From<RemoteProverClientError> for String {
    fn from(err: RemoteProverClientError) -> Self {
        err.to_string()
    }
}

impl RemoteProverClientError {
    /// Creates a custom error using the [`RemoteProverClientError::Other`] variant from an
    /// error message.
    pub fn other(message: impl Into<String>) -> Self {
        let message: String = message.into();
        Self::Other { error_msg: message.into(), source: None }
    }

    /// Creates a custom error using the [`RemoteProverClientError::Other`] variant from an
    /// error message and a source error.
    pub fn other_with_source(
        message: impl Into<String>,
        source: impl CoreError + Send + Sync + 'static,
    ) -> Self {
        let message: String = message.into();
        Self::Other {
            error_msg: message.into(),
            source: Some(Box::new(source)),
        }
    }
}
