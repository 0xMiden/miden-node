mod bootstrap;
mod start;

use std::num::NonZeroUsize;
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use miden_node_utils::clap::GrpcOptionsInternal;
use miden_node_utils::logging::OpenTelemetry;
use miden_node_utils::shutdown::CancellationToken;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey;
use miden_protocol::utils::serde::Deserializable;
use miden_validator::{DataDirectory, ValidatorSigner};

const ENV_DATA_DIRECTORY: &str = "MIDEN_VALIDATOR_DATA_DIRECTORY";
const ENV_LISTEN: &str = "MIDEN_VALIDATOR_LISTEN";
const ENV_KEY: &str = "MIDEN_VALIDATOR_KEY";
const ENV_KMS_KEY_ID: &str = "MIDEN_VALIDATOR_KMS_KEY_ID";
const ENV_GENESIS_CONFIG_FILE: &str = "MIDEN_VALIDATOR_GENESIS_CONFIG_FILE";
const ENV_SQLITE_CONNECTION_POOL_SIZE: &str = "MIDEN_VALIDATOR_SQLITE_CONNECTION_POOL_SIZE";

/// A predefined, insecure validator key for development purposes.
pub(crate) const INSECURE_KEY_HEX: &str =
    "0101010101010101010101010101010101010101010101010101010101010101";

// VALIDATOR COMMAND
// ================================================================================================

#[derive(Parser)]
#[command(version, about, long_about = None)]
pub enum ValidatorCommand {
    /// Bootstraps the genesis block.
    ///
    /// Creates accounts from the genesis configuration, builds the genesis block, signs it with
    /// every validator key, and writes the signed block and account secret files to disk. Also
    /// initializes the validator's database with the genesis block as the chain tip.
    ///
    /// The genesis block is the chain's trust root and must be signed by the complete validator
    /// set committed to by its header, so this command requires signing access to all validator
    /// keys. Only one validator in the set needs to run this form.
    ///
    /// Alternatively, pass `--file` to seed this validator's database from a genesis block that
    /// another validator has already built and signed, without re-signing it. Use this for every
    /// validator other than the one that ran the signing form above.
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
        ///
        /// Cannot be used with `--file`.
        #[arg(long, env = ENV_GENESIS_CONFIG_FILE, value_name = "GENESIS_CONFIG")]
        genesis_config_file: Option<PathBuf>,
        /// Seed this validator's database from an already-signed genesis block file, instead of
        /// building and signing a new one.
        ///
        /// Cannot be used with `--genesis-config-file` or the validator key arguments.
        #[arg(long = "file", value_name = "FILE")]
        genesis_block_file: Option<PathBuf>,
        /// Configuration for the validator keys used to sign the genesis block.
        ///
        /// Ignored when `--file` is used.
        #[command(flatten)]
        validator_keys: ValidatorKeyArgs,
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
                genesis_block_file,
                validator_keys,
            } => {
                bootstrap::bootstrap(
                    &genesis_block_directory,
                    &accounts_directory,
                    &data_directory,
                    sqlite_connection_pool_size,
                    genesis_config_file.as_ref(),
                    genesis_block_file.as_ref(),
                    validator_keys,
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
                ..
            } => {
                let address = listen;

                if let Some(kms_key_id) = kms_key_id {
                    let signer = ValidatorSigner::new_kms(kms_key_id).await?;
                    start::start(
                        address,
                        grpc_options,
                        signer,
                        data_directory,
                        sqlite_connection_pool_size,
                        shutdown,
                    )
                    .await
                } else {
                    let signer = SigningKey::read_from_bytes(hex::decode(validator_key)?.as_ref())?;
                    let signer = ValidatorSigner::new_local(signer);
                    start::start(
                        address,
                        grpc_options,
                        signer,
                        data_directory,
                        sqlite_connection_pool_size,
                        shutdown,
                    )
                    .await
                }
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

// VALIDATOR KEY ARGS
// ================================================================================================

/// Configuration for the validator keys used to sign the genesis block.
///
/// One signer is required for every member of the genesis validator set, so the arguments accept
/// multiple keys (repeat the argument or comma-separate the values).
#[derive(clap::Args)]
#[group(required = false, multiple = false)]
pub struct ValidatorKeyArgs {
    /// Insecure, hex-encoded validator secret keys for development and testing purposes.
    ///
    /// If not provided, a single predefined key is used.
    ///
    /// Cannot be used with `key.kms-id`.
    #[arg(
        long = "key.hex",
        env = ENV_KEY,
        value_name = "VALIDATOR_KEYS",
        default_value = INSECURE_KEY_HEX,
        value_delimiter = ',',
    )]
    pub validator_keys: Vec<String>,
    /// Key IDs for the KMS keys used by the validators to sign blocks.
    ///
    /// Cannot be used with `key.hex`.
    #[arg(
        long = "key.kms-id",
        env = ENV_KMS_KEY_ID,
        value_name = "VALIDATOR_KMS_KEY_IDS",
        value_delimiter = ',',
    )]
    pub validator_kms_key_ids: Vec<String>,
}

impl ValidatorKeyArgs {
    pub async fn into_signers(self) -> anyhow::Result<Vec<ValidatorSigner>> {
        if self.validator_kms_key_ids.is_empty() {
            self.validator_keys
                .iter()
                .map(|key| {
                    let signer = SigningKey::read_from_bytes(hex::decode(key)?.as_ref())?;
                    Ok(ValidatorSigner::new_local(signer))
                })
                .collect()
        } else {
            let mut signers = Vec::with_capacity(self.validator_kms_key_ids.len());
            for kms_key_id in self.validator_kms_key_ids {
                signers.push(ValidatorSigner::new_kms(kms_key_id).await?);
            }
            Ok(signers)
        }
    }
}
