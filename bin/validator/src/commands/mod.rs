mod bootstrap;
mod start;

use std::num::NonZeroUsize;
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use miden_node_utils::clap::GrpcOptionsInternal;
use miden_node_utils::logging::OpenTelemetry;
use miden_node_utils::shutdown::CancellationToken;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{PublicKey, SigningKey};
use miden_protocol::utils::serde::{Deserializable, Serializable};
use miden_validator::{DataDirectory, ValidatorSigner};

const ENV_DATA_DIRECTORY: &str = "MIDEN_VALIDATOR_DATA_DIRECTORY";
const ENV_LISTEN: &str = "MIDEN_VALIDATOR_LISTEN";
const ENV_KEY: &str = "MIDEN_VALIDATOR_KEY";
const ENV_KMS_KEY_ID: &str = "MIDEN_VALIDATOR_KMS_KEY_ID";
const ENV_GENESIS_CONFIG_FILE: &str = "MIDEN_VALIDATOR_GENESIS_CONFIG_FILE";
const ENV_GENESIS_VALIDATOR_PUBKEYS: &str = "MIDEN_VALIDATOR_GENESIS_VALIDATOR_PUBKEYS";
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
    /// this validator's key, and writes the signed block and account secret files to disk. Also
    /// initializes the validator's database with the genesis block as the chain tip.
    ///
    /// The genesis block is the chain's trust root: its header commits to the full validator set
    /// (this validator's key plus the public keys passed via `--validator.pubkey`), but only the
    /// bootstrapping validator signs it. The full set is required to sign from the next block
    /// onwards. Only one validator in the set runs this form, and it only needs signing access
    /// to its own key.
    ///
    /// Alternatively, pass `--file` to seed this validator's database from the genesis block
    /// produced by the signing form above. The block must carry a valid signature from a key in
    /// its committed validator set and is verified, not re-signed. Use this for every validator
    /// other than the one that ran the signing form.
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
        /// Cannot be used with `--genesis-config-file`; the validator key arguments are ignored.
        #[arg(long = "file", value_name = "FILE", conflicts_with = "genesis_config_file")]
        genesis_block_file: Option<PathBuf>,
        /// Configuration for the validator key used to sign the genesis block.
        ///
        /// Ignored when `--file` is used.
        #[command(flatten)]
        validator_keys: ValidatorKeyArgs,
        /// Hex-encoded public keys of the other genesis validators (repeat the argument or
        /// comma-separate the values).
        ///
        /// These keys are committed to by the genesis header alongside this validator's key, so
        /// their signatures are required on every block after genesis. They do not sign the
        /// genesis block itself.
        ///
        /// Ignored when `--file` is used.
        #[arg(
            long = "validator.pubkey",
            env = ENV_GENESIS_VALIDATOR_PUBKEYS,
            value_name = "VALIDATOR_PUBLIC_KEYS",
            value_delimiter = ','
        )]
        genesis_validator_public_keys: Vec<String>,
    },

    /// Prints the hex-encoded public key for the configured validator key.
    ///
    /// Every validator other than the one bootstrapping the genesis block runs this and passes
    /// the printed key to the bootstrapping validator, which commits it to the genesis header
    /// via `bootstrap --validator.pubkey`.
    Pubkey {
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
                genesis_validator_public_keys,
            } => {
                if let Some(genesis_block_file) = genesis_block_file {
                    bootstrap::bootstrap_from_file(
                        &genesis_block_directory,
                        &accounts_directory,
                        &data_directory,
                        sqlite_connection_pool_size,
                        &genesis_block_file,
                    )
                    .await
                } else {
                    let other_validator_keys = genesis_validator_public_keys
                        .iter()
                        .map(|key| {
                            let bytes = hex::decode(key)
                                .context("failed to hex-decode validator public key")?;
                            PublicKey::read_from_bytes(&bytes)
                                .context("failed to parse validator public key")
                        })
                        .collect::<anyhow::Result<Vec<_>>>()?;
                    bootstrap::bootstrap_sign(
                        &genesis_block_directory,
                        &accounts_directory,
                        &data_directory,
                        sqlite_connection_pool_size,
                        genesis_config_file.as_ref(),
                        validator_keys,
                        other_validator_keys,
                    )
                    .await
                }
            },
            Self::Pubkey { validator_keys } => {
                let signer = validator_keys.into_signer().await?;
                println!("{}", hex::encode(signer.public_key().to_bytes()));
                Ok(())
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
            Self::Bootstrap { .. } | Self::Pubkey { .. } | Self::Migrate { .. } => {
                OpenTelemetry::Disabled
            },
        }
    }
}

// VALIDATOR KEY ARGS
// ================================================================================================

/// Configuration for the validator key used to sign the genesis block.
#[derive(clap::Args)]
#[group(required = false, multiple = false)]
pub struct ValidatorKeyArgs {
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
    /// Key ID for the KMS key used by the validator to sign blocks.
    ///
    /// Cannot be used with `key.hex`.
    #[arg(long = "key.kms-id", env = ENV_KMS_KEY_ID, value_name = "VALIDATOR_KMS_KEY_ID")]
    pub kms_key_id: Option<String>,
}

impl ValidatorKeyArgs {
    pub async fn into_signer(self) -> anyhow::Result<ValidatorSigner> {
        if let Some(kms_key_id) = self.kms_key_id {
            ValidatorSigner::new_kms(kms_key_id).await
        } else {
            let signer = SigningKey::read_from_bytes(hex::decode(self.validator_key)?.as_ref())?;
            Ok(ValidatorSigner::new_local(signer))
        }
    }
}
