use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use miden_node_utils::clap::GrpcOptionsInternal;
use miden_node_utils::shutdown::CancellationToken;
use miden_validator::{
    DataDirectory,
    GoldenOperatorKey,
    TransactionInputDecrypter,
    ValidatorServer,
    ValidatorSigner,
};

pub(crate) struct ValidatorKeys {
    pub(crate) signer: ValidatorSigner,
    pub(crate) decrypter: Arc<dyn TransactionInputDecrypter>,
    pub(crate) storage_key: Option<Arc<GoldenOperatorKey>>,
}

// Starts the validator component.
pub async fn start(
    address: SocketAddr,
    grpc_options: GrpcOptionsInternal,
    keys: ValidatorKeys,
    data_directory: PathBuf,
    sqlite_connection_pool_size: NonZeroUsize,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let data_directory =
        DataDirectory::load(data_directory).context("failed to load validator data directory")?;
    ValidatorServer {
        address,
        grpc_options,
        signer: keys.signer,
        decrypter: keys.decrypter,
        storage_key: keys.storage_key,
        data_directory,
        sqlite_connection_pool_size,
    }
    .serve(shutdown)
    .await
    .context("failed while serving validator component")
}
