use thiserror::Error;
use tonic::transport::Error as TransportError;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("an I/O error has occurred")]
    IoError(#[from] std::io::Error),

    #[error("initialisation of the Api has failed: {0}")]
    ApiInitialisationFailed(String),

    #[error("serving the Api server has failed")]
    ApiServeFailed(#[from] TransportError),

    #[error("resolution of the server address has failed: {0}")]
    AddressResolutionFailed(String),

    /// Converting the provided `Endpoint` into a socket address has failed
    #[error("converting the `Endpoint` into a socket address failed: {0}")]
    EndpointToSocketFailed(std::io::Error),

    #[error("connection to the database has failed: {0}")]
    DatabaseConnectionFailed(String),

    #[error("parsing store url failed: {0}")]
    InvalidStoreUrl(String),
}

pub trait ErrorReport: std::error::Error {
    fn as_report(&self) -> String {
        use std::fmt::Write;
        let mut report = self.to_string();

        // SAFETY: write! is suggested by clippy, and is trivially safe usage.
        std::iter::successors(self.source(), |child| child.source())
            .for_each(|source| write!(report, "\nCaused by: {source}").unwrap());

        report
    }
}

impl<T: std::error::Error> ErrorReport for T {}
