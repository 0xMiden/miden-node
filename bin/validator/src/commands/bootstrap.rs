use std::num::NonZeroUsize;
use std::path::Path;

use anyhow::Context;
use miden_node_store::BlockStore;
use miden_node_store::genesis::GenesisBlock;
use miden_node_utils::fs::ensure_empty_directory;
use miden_node_utils::genesis::read_genesis_block;
use miden_validator::DataDirectory;

/// Runs the `bootstrap` command: seeds this validator's database from the genesis block file
/// produced by the `genesis` command.
///
/// The genesis block is the chain's trust root and carries no signatures; it must come from a
/// trusted source. This command verifies the block (via [`GenesisBlock::try_from`]) and persists
/// it as the chain tip.
pub async fn bootstrap(
    data_directory: &Path,
    sqlite_connection_pool_size: NonZeroUsize,
    genesis_block_file: &Path,
) -> anyhow::Result<()> {
    tracing::info!(
        target: miden_validator::LOG_TARGET,
        {
            service.name = "miden-validator",
            service.version = env!("CARGO_PKG_VERSION"),
            genesis.file = %genesis_block_file.display(),
            data.directory = %data_directory.display(),
        },
        "Bootstrapping validator",
    );

    ensure_empty_directory(data_directory)?;
    let dirs = DataDirectory::load(data_directory.to_path_buf())
        .context("failed to load the data directory")?;

    let signed_block =
        read_genesis_block(genesis_block_file).context("failed to read genesis block file")?;
    let genesis_block =
        GenesisBlock::try_from(signed_block).context("genesis block validation failed")?;
    let genesis_commitment = genesis_block.inner().header().commitment();

    let _ = BlockStore::bootstrap(dirs.block_store_dir(), &genesis_block)?;

    let (genesis_header, ..) = genesis_block.into_inner().into_parts();
    let db = miden_validator::db::setup_with_pool_size(
        dirs.database_path(),
        sqlite_connection_pool_size,
    )
    .await
    .context("failed to initialize validator database during bootstrap")?;
    db.write("upsert_block_header", move |tx| {
        miden_validator::db::upsert_block_header(tx, &genesis_header)
    })
    .await
    .context("failed to persist genesis block header as chain tip")?;

    tracing::info!(
        target: miden_validator::LOG_TARGET,
        {
            genesis.commitment = %genesis_commitment,
            data.directory = %data_directory.display(),
        },
        "Validator bootstrap complete",
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bootstrap_writes_all_outputs_before_returning_success() {
        let root = tempfile::tempdir().unwrap();
        let genesis_directory = root.path().join("genesis");
        let accounts_directory = root.path().join("accounts");
        let data_directory = root.path().join("data");

        super::super::genesis::generate(&genesis_directory, &accounts_directory, None)
            .expect("genesis should complete");

        bootstrap(
            &data_directory,
            NonZeroUsize::new(2).unwrap(),
            &genesis_directory.join("genesis.dat"),
        )
        .await
        .expect("bootstrap should complete");

        assert!(genesis_directory.join("genesis.dat").is_file());
        assert!(data_directory.join("validator.sqlite3").is_file());
        assert!(
            fs_err::read_dir(accounts_directory)
                .expect("accounts directory should be readable")
                .next()
                .is_some(),
            "genesis should write generated account files",
        );
    }
}
