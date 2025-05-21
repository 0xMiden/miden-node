use thiserror::Error;

#[derive(Debug, Error)]
pub enum RpcError {
    #[error("failed to connect to the api server: {0}")]
    ConnectionError(#[from] tonic::transport::Error),
}
