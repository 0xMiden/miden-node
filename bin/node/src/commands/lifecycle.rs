use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::ArgGroup;
use miden_node_store::genesis::GenesisBlock;
use miden_node_store::{DataDirectory, Db, State};
use miden_node_utils::fs::ensure_empty_directory;
use miden_node_utils::genesis::{
    OfficialNetwork,
    fetch_signed_genesis_block,
    read_signed_genesis_block,
};

use super::ENV_DATA_DIRECTORY;

// BOOTSTRAP
// ================================================================================================

#[derive(clap::Args, Clone, Debug)]
#[command(group(
    ArgGroup::new("genesis_block_source")
        .required(true)
        .multiple(false)
        .args(["genesis_block_file", "network"])
))]
pub struct BootstrapCommand {
    /// Directory to initialize with the node's local data storage.
    #[arg(long, env = ENV_DATA_DIRECTORY, value_name = "DIR")]
    data_directory: PathBuf,

    /// Bootstrap from a trusted genesis block file.
    #[arg(long = "genesis", value_name = "FILE")]
    genesis_block_file: Option<PathBuf>,

    /// Bootstrap for an official Miden network.
    #[arg(long, value_enum, value_name = "NETWORK")]
    network: Option<OfficialNetwork>,
}

impl BootstrapCommand {
    pub async fn handle(self) -> anyhow::Result<()> {
        ensure_empty_directory(&self.data_directory)?;
        let genesis_block =
            read_bootstrap_genesis_block(self.genesis_block_file.as_deref(), self.network).await?;
        State::bootstrap(genesis_block, &self.data_directory)
    }
}

/// Reads the genesis block from the configured source and validates it.
async fn read_bootstrap_genesis_block(
    genesis_block_file: Option<&Path>,
    network: Option<OfficialNetwork>,
) -> anyhow::Result<GenesisBlock> {
    let signed_block = match (genesis_block_file, network) {
        (Some(path), None) => read_signed_genesis_block(path)?,
        (None, Some(network)) => fetch_signed_genesis_block(network).await?,
        _ => unreachable!("clap requires exactly one genesis block source"),
    };
    GenesisBlock::try_from(signed_block)
}

// MIGRATE
// ================================================================================================

#[derive(clap::Args, Clone, Debug)]
pub struct MigrateCommand {
    /// Directory containing the node's local data storage.
    #[arg(long, env = ENV_DATA_DIRECTORY, value_name = "DIR")]
    data_directory: PathBuf,
}

impl MigrateCommand {
    pub fn handle(self) -> anyhow::Result<()> {
        let data_directory =
            DataDirectory::load(self.data_directory.clone()).with_context(|| {
                format!("failed to load data directory at {}", self.data_directory.display())
            })?;

        Db::migrate(data_directory.database_path())
            .context("failed to apply store database migrations")?;

        Ok(())
    }
}
