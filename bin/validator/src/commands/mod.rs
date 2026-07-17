mod bootstrap;
mod start;

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use miden_node_utils::clap::GrpcOptionsInternal;
use miden_node_utils::logging::OpenTelemetry;
use miden_node_utils::shutdown::CancellationToken;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey;
use miden_protocol::crypto::dsa::eddsa_25519_sha512::KeyExchangeKey;
use miden_protocol::utils::serde::Deserializable;
use miden_validator::{
    DataDirectory,
    LOG_TARGET,
    LocalX25519TransactionInputDecryptor,
    TransactionInputDecryptor,
    ValidatorSigner,
};

const ENV_DATA_DIRECTORY: &str = "MIDEN_VALIDATOR_DATA_DIRECTORY";
const ENV_LISTEN: &str = "MIDEN_VALIDATOR_LISTEN";
const ENV_KEY: &str = "MIDEN_VALIDATOR_KEY";
const ENV_KMS_KEY_ID: &str = "MIDEN_VALIDATOR_KMS_KEY_ID";
const ENV_ENCRYPTION_KEY: &str = "MIDEN_VALIDATOR_ENCRYPTION_KEY";
const ENV_GENESIS_CONFIG_FILE: &str = "MIDEN_VALIDATOR_GENESIS_CONFIG_FILE";
const ENV_SQLITE_CONNECTION_POOL_SIZE: &str = "MIDEN_VALIDATOR_SQLITE_CONNECTION_POOL_SIZE";

/// A predefined, insecure validator key for development purposes.
pub(crate) const INSECURE_KEY_HEX: &str =
    "0101010101010101010101010101010101010101010101010101010101010101";

/// A predefined, insecure shared transaction encryption key for development purposes.
pub(crate) const INSECURE_ENCRYPTION_KEY_HEX: &str =
    "0202020202020202020202020202020202020202020202020202020202020202";

// VALIDATOR COMMAND
// ================================================================================================

#[derive(Parser)]
#[command(version, about, long_about = None)]
pub enum ValidatorCommand {
    /// Bootstraps the genesis block.
    ///
    /// Creates accounts from the genesis configuration, builds and signs the genesis block,
    /// and writes the signed block and account secret files to disk. Also initializes the
    /// validator's database with the genesis block as the chain tip.
    Bootstrap {
        /// Directory in which to write the genesis block file.
        #[arg(long, value_name = "DIR")]
        genesis_block_directory: PathBuf,
        /// Directory to write the account secret files (.mac) to.
        #[arg(long, value_name = "DIR")]
        accounts_directory: PathBuf,
        /// Directory in which to store the validator's database.
        #[arg(long, env = ENV_DATA_DIRECTORY, value_name = "DIR")]
        data_directory: PathBuf,
        /// Maximum number of SQLite connections in the validator database connection pool.
        #[arg(
            long = "sqlite.connection_pool_size",
            env = ENV_SQLITE_CONNECTION_POOL_SIZE,
            default_value_t = miden_node_db::default_connection_pool_size(),
            value_name = "NUM"
        )]
        sqlite_connection_pool_size: NonZeroUsize,
        /// Use the given configuration file to construct the genesis state from.
        #[arg(long, env = ENV_GENESIS_CONFIG_FILE, value_name = "GENESIS_CONFIG")]
        genesis_config_file: Option<PathBuf>,
        /// Configuration for the Validator key used to sign the genesis block.
        #[command(flatten)]
        validator_key: ValidatorKey,
    },

    /// Applies pending validator database migrations.
    ///
    /// Cannot be run on an empty data directory; run `bootstrap` first.
    Migrate {
        /// Directory in which to store the validator's data.
        #[arg(long, env = ENV_DATA_DIRECTORY, value_name = "DIR")]
        data_directory: PathBuf,
    },

    /// Starts the validator component.
    Start {
        /// Socket address at which to serve the gRPC API.
        #[arg(long = "listen", env = ENV_LISTEN, value_name = "LISTEN")]
        listen: std::net::SocketAddr,

        #[command(flatten)]
        grpc_options: GrpcOptionsInternal,

        /// Maximum number of SQLite connections in the validator database connection pool.
        #[arg(
            long = "sqlite.connection_pool_size",
            env = ENV_SQLITE_CONNECTION_POOL_SIZE,
            default_value_t = miden_node_db::default_connection_pool_size(),
            value_name = "NUM"
        )]
        sqlite_connection_pool_size: NonZeroUsize,

        /// Directory in which to store the validator's data.
        #[arg(long, env = ENV_DATA_DIRECTORY, value_name = "DIR")]
        data_directory: PathBuf,

        /// Insecure, hex-encoded validator secret key for development and testing purposes.
        ///
        /// If not provided, a predefined key is used.
        ///
        /// Cannot be used with `key.kms-id`.
        #[arg(
            long = "key.hex",
            env = ENV_KEY,
            value_name = "VALIDATOR_KEY",
            default_value = INSECURE_KEY_HEX,
            group = "key"
        )]
        validator_key: String,

        /// Key ID for the KMS key used by validator to sign blocks.
        ///
        /// Cannot be used with `key.hex`.
        #[arg(
            long = "key.kms-id",
            env = ENV_KMS_KEY_ID,
            value_name = "VALIDATOR_KMS_KEY_ID",
            group = "key"
        )]
        kms_key_id: Option<String>,

        /// Hex-encoded shared secret of the transaction encryption key.
        ///
        /// Unlike the per-validator signing key, this value must be identical across every
        /// validator in the set.
        ///
        /// If not provided, a predefined insecure key is used.
        #[arg(
            long = "encryption-key.hex",
            env = ENV_ENCRYPTION_KEY,
            value_name = "VALIDATOR_ENCRYPTION_KEY",
            default_value = INSECURE_ENCRYPTION_KEY_HEX
        )]
        encryption_key: String,
    },
}

impl ValidatorCommand {
    pub async fn handle(self, shutdown: CancellationToken) -> anyhow::Result<()> {
        match self {
            Self::Bootstrap {
                genesis_block_directory,
                accounts_directory,
                data_directory,
                sqlite_connection_pool_size,
                genesis_config_file,
                validator_key,
            } => {
                bootstrap::bootstrap(
                    &genesis_block_directory,
                    &accounts_directory,
                    &data_directory,
                    sqlite_connection_pool_size,
                    genesis_config_file.as_ref(),
                    validator_key,
                )
                .await
            },
            Self::Migrate { data_directory } => {
                let data_dir = DataDirectory::load_server(data_directory)
                    .context("failed to load validator data directory")?;
                miden_validator::db::migrate(data_dir.database_path())
                    .context("failed to apply validator database migrations")?;
                Ok(())
            },
            Self::Start {
                listen,
                grpc_options,
                validator_key,
                data_directory,
                kms_key_id,
                sqlite_connection_pool_size,
                encryption_key,
                ..
            } => {
                let address = listen;

                // Unlike the signing key, whose insecure default is caught at startup against the
                // chain's committed validator key, nothing cross-checks the encryption key. Warn
                // loudly so the default never runs in production unnoticed.
                if encryption_key == INSECURE_ENCRYPTION_KEY_HEX {
                    tracing::warn!(
                        target: LOG_TARGET,
                        "Using the predefined, insecure transaction encryption key, configure \
                         --encryption-key.hex for production deployments"
                    );
                }

                let encryption_key_bytes = hex::decode(encryption_key)
                    .context("failed to decode the encryption key hex")?;
                let encryption_key = KeyExchangeKey::read_from_bytes(&encryption_key_bytes)
                    .context("failed to construct the encryption key")?;
                let decryptor: Arc<dyn TransactionInputDecryptor> =
                    Arc::new(LocalX25519TransactionInputDecryptor::new(encryption_key));

                let signer = if let Some(kms_key_id) = kms_key_id {
                    ValidatorSigner::new_kms(kms_key_id).await?
                } else {
                    let signer = SigningKey::read_from_bytes(hex::decode(validator_key)?.as_ref())?;
                    ValidatorSigner::new_local(signer)
                };

                start::start(
                    address,
                    grpc_options,
                    signer,
                    decryptor,
                    data_directory,
                    sqlite_connection_pool_size,
                    shutdown,
                )
                .await
            },
        }
    }

    pub fn open_telemetry(&self) -> OpenTelemetry {
        match self {
            Self::Start { .. } => OpenTelemetry::from_env().with_name("validator"),
            Self::Bootstrap { .. } | Self::Migrate { .. } => OpenTelemetry::Disabled,
        }
    }
}

// VALIDATOR KEY
// ================================================================================================

/// Configuration for the Validator key used to sign blocks.
#[derive(clap::Args)]
#[group(required = false, multiple = false)]
pub struct ValidatorKey {
    /// Insecure, hex-encoded validator secret key for development and testing purposes.
    ///
    /// If not provided, a predefined key is used.
    ///
    /// Cannot be used with `key.kms-id`.
    #[arg(
        long = "key.hex",
        env = ENV_KEY,
        value_name = "VALIDATOR_KEY",
        default_value = INSECURE_KEY_HEX,
    )]
    pub validator_key: String,
    /// Key ID for the KMS key used by validator to sign blocks.
    ///
    /// Cannot be used with `key.hex`.
    #[arg(
        long = "key.kms-id",
        env = ENV_KMS_KEY_ID,
        value_name = "VALIDATOR_KMS_KEY_ID",
    )]
    pub validator_kms_key_id: Option<String>,
}

impl ValidatorKey {
    pub async fn into_signer(self) -> anyhow::Result<ValidatorSigner> {
        if let Some(kms_key_id) = self.validator_kms_key_id {
            Ok(ValidatorSigner::new_kms(kms_key_id).await?)
        } else {
            let signer = SigningKey::read_from_bytes(hex::decode(self.validator_key)?.as_ref())?;
            Ok(ValidatorSigner::new_local(signer))
        }
    }
}
