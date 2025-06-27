use axum::http::uri::InvalidUri;
use thiserror::Error;

// TX PROVER SERVICE ERROR
// ================================================================================================

#[derive(Debug, Error)]
pub enum RemoteProverError {
    #[error("invalid uri {1}")]
    InvalidURI(#[source] InvalidUri, String),
    #[error("failed to connect to worker {1}")]
    ConnectionFailed(#[source] tonic::transport::Error, String),
    #[error("failed to create backend for worker")]
    BackendCreationFailed(#[source] Box<pingora::Error>),
    #[error("failed to setup pingora: {0}")]
    PingoraConfigFailed(String),
    #[error("failed to parse int: {0}")]
    ParseError(#[from] std::num::ParseIntError),
    #[error("port {1} is already in use: {0}")]
    PortAlreadyInUse(#[source] std::io::Error, u16),
}

impl From<RemoteProverError> for String {
    fn from(err: RemoteProverError) -> Self {
        err.to_string()
    }
}
