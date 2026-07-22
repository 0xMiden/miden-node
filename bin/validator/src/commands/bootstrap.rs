use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use anyhow::Context;
use miden_node_store::BlockStore;
use miden_node_store::genesis::GenesisBlock;
use miden_node_store::genesis::config::{AccountFileWithName, GenesisConfig};
use miden_node_utils::fs::ensure_empty_directory;
use miden_node_utils::genesis::read_signed_genesis_block;
use miden_protocol::block::BlockSignatures;
use miden_protocol::utils::serde::Serializable;
use miden_validator::DataDirectory;

use super::ValidatorSigningKey;

/// Runs the signing form of `bootstrap`: builds and signs the genesis block with this
/// validator's key.
///
/// Builds the genesis state from the given configuration — or from [`GenesisConfig::default`]
/// when `None`, as in local development where the built-in single-validator configuration
/// suffices — writes the account secret files, signs the genesis block with this validator's
/// key, and persists the block as the chain tip.
///
/// The genesis header commits to the full validator set, taken from the `validators` public
/// keys in the genesis configuration (defaulting to this validator's key alone). Only this
/// validator signs the genesis block; the full set is required to sign from the next block
/// onwards, so bootstrapping does not need signing access to the other validators' keys.
///
/// Every other validator seeds from this form's output via [`bootstrap_from_file`].
pub async fn bootstrap_sign(
    genesis_block_directory: &Path,
    accounts_directory: &Path,
    data_directory: &Path,
    sqlite_connection_pool_size: NonZeroUsize,
    genesis_config: Option<&PathBuf>,
    signing_key: ValidatorSigningKey,
) -> anyhow::Result<()> {
    let dirs = load_bootstrap_dirs(genesis_block_directory, accounts_directory, data_directory)?;

    let config = genesis_config
        .map(|file_path| {
            GenesisConfig::read_toml_file(file_path).with_context(|| {
                format!("failed to parse genesis config from file {}", file_path.display())
            })
        })
        .transpose()?
        .unwrap_or_default();

    let signer = signing_key.into_signer().await?;
    let (genesis_state, secrets) = config.into_state(signer.public_key())?;

    for item in secrets.as_account_files(&genesis_state) {
        let AccountFileWithName { account_file, name } = item?;
        let account_path = dirs.accounts_dir().expect("bootstrap directories").join(name);
        // Do not override existing keys.
        fs_err::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&account_path)
            .context("key file already exists")?;
        account_file.write(account_path)?;
    }

    let unsigned_genesis_block = genesis_state
        .into_unsigned_block()
        .context("failed to build the unsigned genesis block")?;

    // Sign the genesis block with this validator's key only. The other validators' keys are
    // committed to by the genesis header and must sign from the next block onwards.
    let signature = signer
        .sign_commitment(unsigned_genesis_block.header().commitment())
        .await
        .context("failed to sign the genesis block")?;
    let signatures = BlockSignatures::new(vec![signature])
        .context("failed to build the genesis block signatures")?;

    let genesis_block = unsigned_genesis_block
        .into_block(signatures)
        .context("failed to build the genesis block")?;

    persist_genesis(genesis_block, dirs, sqlite_connection_pool_size).await
}

/// Runs the seeding form of `bootstrap`: initializes this validator from the genesis block
/// produced by the signing form ([`bootstrap_sign`]), without re-signing it.
///
/// The genesis block is the chain's trust root and carries a single signature from the
/// bootstrapping validator, verified against the validator set committed to by its header
/// (via [`GenesisBlock::try_from`]). This form verifies the block and persists it as the
/// chain tip.
pub async fn bootstrap_from_file(
    genesis_block_directory: &Path,
    accounts_directory: &Path,
    data_directory: &Path,
    sqlite_connection_pool_size: NonZeroUsize,
    genesis_block_file: &Path,
) -> anyhow::Result<()> {
    let dirs = load_bootstrap_dirs(genesis_block_directory, accounts_directory, data_directory)?;

    let signed_block = read_signed_genesis_block(genesis_block_file)
        .context("failed to read genesis block file")?;
    let genesis_block =
        GenesisBlock::try_from(signed_block).context("genesis block validation failed")?;

    persist_genesis(genesis_block, dirs, sqlite_connection_pool_size).await
}

/// Empties the bootstrap directories and loads them as a [`DataDirectory`].
fn load_bootstrap_dirs(
    genesis_block_directory: &Path,
    accounts_directory: &Path,
    data_directory: &Path,
) -> anyhow::Result<DataDirectory> {
    for directory in [accounts_directory, genesis_block_directory, data_directory] {
        ensure_empty_directory(directory)?;
    }

    DataDirectory::load_bootstrap(
        genesis_block_directory.to_path_buf(),
        accounts_directory.to_path_buf(),
        data_directory.to_path_buf(),
    )
    .context("failed to load bootstrap directories")
}

/// Writes the genesis block file, bootstraps the block store, and initializes the validator's
/// database with the genesis block as the chain tip.
async fn persist_genesis(
    genesis_block: GenesisBlock,
    dirs: DataDirectory,
    sqlite_connection_pool_size: NonZeroUsize,
) -> anyhow::Result<()> {
    let block_bytes = genesis_block.inner().to_bytes();
    fs_err::write(dirs.genesis_block_path().expect("bootstrap directories"), block_bytes)
        .context("failed to write genesis block")?;

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
