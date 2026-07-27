use std::num::NonZeroUsize;
use std::path::Path;

use anyhow::Context;
use miden_node_store::BlockStore;
use miden_node_store::genesis::GenesisBlock;
use miden_node_utils::fs::ensure_empty_directory;
use miden_node_utils::genesis::read_signed_genesis_block;
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
    ensure_empty_directory(data_directory)?;
    let dirs = DataDirectory::load(data_directory.to_path_buf())
        .context("failed to load the data directory")?;

    let signed_block = read_signed_genesis_block(genesis_block_file)
        .context("failed to read genesis block file")?;
    let genesis_block =
        GenesisBlock::try_from(signed_block).context("genesis block validation failed")?;

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

    Ok(())
}
