use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use anyhow::Context;
use miden_node_store::BlockStore;
use miden_node_store::genesis::config::{AccountFileWithName, GenesisConfig};
use miden_node_utils::fs::ensure_empty_directory;
use miden_protocol::utils::serde::Serializable;
use miden_validator::{DataDirectory, ValidatorSigner};

use super::ValidatorSigningKey;

struct BootstrapSummary {
    genesis_commitment: miden_protocol::Word,
    account_count: usize,
}

// Bootstraps the validator component.
pub async fn bootstrap(
    genesis_block_directory: &Path,
    accounts_directory: &Path,
    data_directory: &Path,
    sqlite_connection_pool_size: NonZeroUsize,
    genesis_config: Option<&PathBuf>,
    signing_key: ValidatorSigningKey,
) -> anyhow::Result<()> {
    tracing::info!(
        target: miden_validator::LOG_TARGET,
        {
            service.name = "miden-validator",
            service.version = env!("CARGO_PKG_VERSION"),
            genesis.config.source = if genesis_config.is_some() { "file" } else { "default" },
            genesis.config.path = %genesis_config.map_or_else(
                || "default".to_owned(),
                |path| path.display().to_string(),
            ),
            genesis.directory = %genesis_block_directory.display(),
            accounts.directory = %accounts_directory.display(),
            data.directory = %data_directory.display(),
        },
        "Bootstrapping validator",
    );

    let config = genesis_config
        .map(|file_path| {
            GenesisConfig::read_toml_file(file_path).with_context(|| {
                format!("failed to parse genesis config from file {}", file_path.display())
            })
        })
        .transpose()?
        .unwrap_or_default();

    for directory in [accounts_directory, genesis_block_directory, data_directory] {
        ensure_empty_directory(directory)?;
    }

    let signer = signing_key.into_signer().await?;
    let dirs = DataDirectory::load_bootstrap(
        genesis_block_directory.to_path_buf(),
        accounts_directory.to_path_buf(),
        data_directory.to_path_buf(),
    )
    .context("failed to load bootstrap directories")?;
    let summary =
        build_and_write_genesis(config, signer, dirs, sqlite_connection_pool_size).await?;

    tracing::info!(
        target: miden_validator::LOG_TARGET,
        {
            genesis.commitment = %summary.genesis_commitment,
            genesis.accounts.count = summary.account_count,
            genesis.file = %genesis_block_directory.join("genesis.dat").display(),
            accounts.directory = %accounts_directory.display(),
            data.directory = %data_directory.display(),
        },
        "Validator bootstrap complete",
    );

    Ok(())
}

/// Builds the genesis state, writes account secret files, signs the genesis block, writes it to
/// disk, and initializes the validator's database with the genesis block as the chain tip.
async fn build_and_write_genesis(
    config: GenesisConfig,
    signer: ValidatorSigner,
    dirs: DataDirectory,
    sqlite_connection_pool_size: NonZeroUsize,
) -> anyhow::Result<BootstrapSummary> {
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
    let signature = signer
        .sign_commitment(unsigned_genesis_block.header().commitment())
        .await
        .context("failed to sign the genesis block")?;
    let genesis_block = unsigned_genesis_block
        .into_block(signature)
        .context("failed to build the genesis block")?;
    let genesis_commitment = genesis_block.inner().header().commitment();
    let account_count = genesis_block.inner().body().updated_accounts().len();

    let block_bytes = genesis_block.inner().to_bytes();
    fs_err::write(dirs.genesis_block_path().expect("bootstrap directories"), block_bytes)
        .context("failed to write genesis block")?;

    let _ = BlockStore::bootstrap(dirs.block_store_dir(), &genesis_block)?;

    let (genesis_header, ..) = genesis_block.into_inner().into_parts();
    let (writer, _reader) = miden_validator::db::setup_with_pool_size(
        dirs.database_path(),
        sqlite_connection_pool_size,
    )
    .await
    .context("failed to initialize validator database during bootstrap")?;
    writer
        .write("upsert_block_header", move |tx| {
            miden_validator::db::upsert_block_header(tx, &genesis_header)
        })
        .await
        .context("failed to persist genesis block header as chain tip")?;

    Ok(BootstrapSummary { genesis_commitment, account_count })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::INSECURE_SIGNING_KEY_HEX;

    #[tokio::test]
    async fn bootstrap_writes_all_outputs_before_returning_success() {
        let root = tempfile::tempdir().unwrap();
        let genesis_directory = root.path().join("genesis");
        let accounts_directory = root.path().join("accounts");
        let data_directory = root.path().join("data");
        let validator_key = ValidatorSigningKey {
            signing_key: INSECURE_SIGNING_KEY_HEX.to_owned(),
            signing_key_kms_id: None,
        };

        bootstrap(
            &genesis_directory,
            &accounts_directory,
            &data_directory,
            NonZeroUsize::new(2).unwrap(),
            None,
            validator_key,
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
            "bootstrap should write generated account files",
        );
    }
}
