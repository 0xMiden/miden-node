use thiserror::Error;

/// Error type for client operations.
#[derive(Debug, Error)]
pub enum ClientError {
    /// gRPC-level error from tonic.
    #[error("gRPC error: {0}")]
    GrpcError(Box<tonic::Status>),

    /// Transport-level error (connection, network, etc.).
    #[error("Transport error: {0}")]
    Transport(#[from] tonic::transport::Error),

    /// Invalid endpoint URL or configuration.
    #[error("Invalid endpoint: {0}")]
    InvalidEndpoint(String),

    /// Client configuration error.
    #[error("Configuration error: {0}")]
    Configuration(String),

    /// Invalid metadata.
    #[error("Invalid metadata: {0}")]
    InvalidMetadata(String),

    /// Missing address configuration.
    #[error("No address configured. Use with_address() to set the endpoint URL.")]
    MissingAddress,
}

impl From<tonic::Status> for ClientError {
    fn from(status: tonic::Status) -> Self {
        Self::GrpcError(Box::new(status))
    }
}
