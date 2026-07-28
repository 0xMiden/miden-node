mod bootstrap;
mod genesis;
mod start;

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use base64::Engine;
use clap::Parser;
use miden_node_utils::clap::GrpcOptionsInternal;
use miden_node_utils::genesis::INSECURE_VALIDATOR_SIGNING_KEY_HEX;
use miden_node_utils::logging::OpenTelemetry;
use miden_node_utils::shutdown::CancellationToken;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey;
use miden_protocol::crypto::dsa::eddsa_25519_sha512::KeyExchangeKey;
use miden_protocol::utils::serde::{Deserializable, Serializable};
use miden_validator::{
    DataDirectory,
    LOG_TARGET,
    LocalX25519TransactionInputDecrypter,
    TransactionInputDecrypter,
    ValidatorSigner,
};

const ENV_DATA_DIRECTORY: &str = "MIDEN_VALIDATOR_DATA_DIRECTORY";
const ENV_LISTEN: &str = "MIDEN_VALIDATOR_LISTEN";
const ENV_SIGNING_KEY: &str = "MIDEN_VALIDATOR_SIGNING_KEY";
const ENV_SIGNING_KEY_KMS_ID: &str = "MIDEN_VALIDATOR_SIGNING_KEY_KMS_ID";
const ENV_ENCRYPTION_KEY: &str = "MIDEN_VALIDATOR_ENCRYPTION_KEY";
const ENV_ENCRYPTION_KEY_KMS_CIPHERTEXT: &str = "MIDEN_VALIDATOR_ENCRYPTION_KEY_KMS_CIPHERTEXT";
const ENV_GENESIS_CONFIG: &str = "MIDEN_VALIDATOR_GENESIS_CONFIG";
const ENV_SQLITE_CONNECTION_POOL_SIZE: &str = "MIDEN_VALIDATOR_SQLITE_CONNECTION_POOL_SIZE";

/// A predefined, insecure shared transaction encryption key for development purposes.
pub(crate) const INSECURE_ENCRYPTION_KEY_HEX: &str =
    "0202020202020202020202020202020202020202020202020202020202020202";

// VALIDATOR COMMAND
// ================================================================================================

#[derive(Parser)]
#[command(version, about, long_about = None)]
pub enum ValidatorCommand {
    /// Builds the genesis block from a genesis configuration.
    ///
    /// Creates accounts from the genesis configuration, builds the genesis block, and writes the
    /// block and account secret files to disk.
    ///
    /// The genesis block is the chain's trust root and is not signed: its header commits to the
    /// full validator set — the `validators` public keys in the genesis configuration, defaulting
    /// to the predefined, insecure development key for local development — and that set is
    /// required to sign every block after genesis. Building the genesis block needs no signing
    /// access to any validator's key, so one operator — who need not be a validator — runs this
    /// once and distributes the genesis block file.
    ///
    /// Every validator then seeds its database from the genesis block file with `bootstrap`.
    Genesis {
        /// Directory in which to write the genesis block file.
        #[arg(long, value_name = "DIR")]
        genesis_block_directory: PathBuf,
        /// Directory to write the account secret files (.mac) to.
        #[arg(long, value_name = "DIR")]
        accounts_directory: PathBuf,
        /// Use the given configuration file to construct the genesis state from.
        ///
        /// If not provided, the built-in single-validator development configuration is used.
        #[arg(long = "config", env = ENV_GENESIS_CONFIG, value_name = "GENESIS_CONFIG")]
        genesis_config_file: Option<PathBuf>,
    },

    /// Seeds this validator's database from a genesis block file.
    ///
    /// Every validator runs this once before `start`, against the genesis block file produced by
    /// the `genesis` command. The genesis block is the chain's trust root and carries no
    /// signatures; the file must come from a trusted source.
    Bootstrap {
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
        /// Genesis block file to seed this validator's database from.
        #[arg(long = "genesis", value_name = "FILE")]
        genesis_block_file: PathBuf,
    },

    /// Prints the hex-encoded public key for the configured validator signing key.
    ///
    /// Every validator operator runs this and sends the printed key to whoever composes the
    /// genesis configuration, which commits the full set to the genesis header via its
    /// `validators` list.
    Pubkey {
        #[command(flatten)]
        signing_key: ValidatorSigningKey,
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
        /// Cannot be used with `signing-key.kms-id`.
        #[arg(
            long = "signing-key.hex",
            env = ENV_SIGNING_KEY,
            value_name = "VALIDATOR_SIGNING_KEY",
            default_value = INSECURE_VALIDATOR_SIGNING_KEY_HEX,
            group = "signing_key_source"
        )]
        signing_key: String,

        /// Key ID for the KMS key used by validator to sign blocks.
        ///
        /// Cannot be used with `signing-key.hex`.
        #[arg(
            long = "signing-key.kms-id",
            env = ENV_SIGNING_KEY_KMS_ID,
            value_name = "VALIDATOR_SIGNING_KEY_KMS_ID",
            group = "signing_key_source"
        )]
        signing_key_kms_id: Option<String>,

        /// Hex-encoded shared secret of the transaction encryption key.
        ///
        /// Unlike the per-validator signing key, this value must be identical across every
        /// validator in the set.
        ///
        /// If not provided, a predefined insecure key is used.
        ///
        /// Cannot be used with `encryption-key.kms-ciphertext`.
        #[arg(
            long = "encryption-key.hex",
            env = ENV_ENCRYPTION_KEY,
            value_name = "VALIDATOR_ENCRYPTION_KEY",
            default_value = INSECURE_ENCRYPTION_KEY_HEX,
            group = "encryption_key_source"
        )]
        encryption_key: String,

        /// Base64-encoded KMS ciphertext of the shared transaction encryption key, as returned
        /// by `kms:Encrypt`.
        ///
        /// The wrapped key material is recovered at startup with `kms:Decrypt`. The ciphertext
        /// must have been produced by `kms:Encrypt` under a symmetric KMS key, whose ID is
        /// embedded in the ciphertext blob.
        ///
        /// Cannot be used with `encryption-key.hex`.
        #[arg(
            long = "encryption-key.kms-ciphertext",
            env = ENV_ENCRYPTION_KEY_KMS_CIPHERTEXT,
            value_name = "VALIDATOR_ENCRYPTION_KEY_KMS_CIPHERTEXT",
            group = "encryption_key_source"
        )]
        encryption_key_kms_ciphertext: Option<String>,
    },
}

impl ValidatorCommand {
    pub async fn handle(self, shutdown: CancellationToken) -> anyhow::Result<()> {
        match self {
            Self::Genesis {
                genesis_block_directory,
                accounts_directory,
                genesis_config_file,
            } => genesis::generate(
                &genesis_block_directory,
                &accounts_directory,
                genesis_config_file.as_ref(),
            ),
            Self::Bootstrap {
                data_directory,
                sqlite_connection_pool_size,
                genesis_block_file,
            } => {
                bootstrap::bootstrap(
                    &data_directory,
                    sqlite_connection_pool_size,
                    &genesis_block_file,
                )
                .await
            },
            Self::Pubkey { signing_key } => {
                let signer = signing_key.into_signer().await?;
                println!("{}", hex::encode(signer.public_key().to_bytes()));
                Ok(())
            },
            Self::Migrate { data_directory } => {
                let data_dir = DataDirectory::load(data_directory)
                    .context("failed to load validator data directory")?;
                miden_validator::db::migrate(data_dir.database_path())
                    .context("failed to apply validator database migrations")?;
                Ok(())
            },
            Self::Start {
                listen,
                grpc_options,
                signing_key,
                data_directory,
                signing_key_kms_id,
                sqlite_connection_pool_size,
                encryption_key,
                encryption_key_kms_ciphertext,
                ..
            } => {
                let address = listen;

                let encryption_key_bytes = if let Some(ciphertext) = encryption_key_kms_ciphertext {
                    let ciphertext =
                        base64::engine::general_purpose::STANDARD
                            .decode(ciphertext)
                            .context("failed to decode the encryption key KMS ciphertext base64")?;
                    miden_validator::decrypt_key_material(ciphertext)
                        .await
                        .context("failed to decrypt the encryption key with KMS")?
                } else {
                    // Unlike the signing key, whose insecure default is caught at startup against
                    // the chain's committed validator key, nothing cross-checks the encryption key.
                    // Warn loudly so the default never runs in production unnoticed.
                    if encryption_key == INSECURE_ENCRYPTION_KEY_HEX {
                        tracing::warn!(
                            target: LOG_TARGET,
                            "Using the predefined, insecure transaction encryption key, configure \
                             --encryption-key.hex or --encryption-key.kms-ciphertext for \
                             production deployments"
                        );
                    }

                    hex::decode(encryption_key)
                        .context("failed to decode the encryption key hex")?
                };
                let encryption_key = KeyExchangeKey::read_from_bytes(&encryption_key_bytes)
                    .context("failed to construct the encryption key")?;
                let decrypter: Arc<dyn TransactionInputDecrypter> =
                    Arc::new(LocalX25519TransactionInputDecrypter::new(encryption_key));

                let signer = if let Some(kms_key_id) = signing_key_kms_id {
                    ValidatorSigner::new_kms(kms_key_id).await?
                } else {
                    let signer = SigningKey::read_from_bytes(hex::decode(signing_key)?.as_ref())?;
                    ValidatorSigner::new_local(signer)
                };

                start::start(
                    address,
                    grpc_options,
                    signer,
                    decrypter,
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
            Self::Genesis { .. }
            | Self::Bootstrap { .. }
            | Self::Pubkey { .. }
            | Self::Migrate { .. } => OpenTelemetry::Disabled,
        }
    }
}

// VALIDATOR SIGNING KEY
// ================================================================================================

/// Configuration for the validator signing key used to sign blocks.
#[derive(clap::Args)]
#[group(required = false, multiple = false)]
pub struct ValidatorSigningKey {
    /// Insecure, hex-encoded validator secret key for development and testing purposes.
    ///
    /// If not provided, a predefined key is used.
    ///
    /// Cannot be used with `signing-key.kms-id`.
    #[arg(
        long = "signing-key.hex",
        env = ENV_SIGNING_KEY,
        value_name = "VALIDATOR_SIGNING_KEY",
        default_value = INSECURE_VALIDATOR_SIGNING_KEY_HEX,
    )]
    pub signing_key: String,
    /// Key ID for the KMS key used by validator to sign blocks.
    ///
    /// Cannot be used with `signing-key.hex`.
    #[arg(
        long = "signing-key.kms-id",
        env = ENV_SIGNING_KEY_KMS_ID,
        value_name = "VALIDATOR_SIGNING_KEY_KMS_ID",
    )]
    pub signing_key_kms_id: Option<String>,
}

impl ValidatorSigningKey {
    pub async fn into_signer(self) -> anyhow::Result<ValidatorSigner> {
        if let Some(kms_key_id) = self.signing_key_kms_id {
            Ok(ValidatorSigner::new_kms(kms_key_id).await?)
        } else {
            let signer = SigningKey::read_from_bytes(hex::decode(self.signing_key)?.as_ref())?;
            Ok(ValidatorSigner::new_local(signer))
        }
    }
}

// TESTS
// ================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const BASE_START_ARGS: [&str; 6] = [
        "miden-validator",
        "start",
        "--listen",
        "127.0.0.1:50101",
        "--data-directory",
        "/tmp/validator-data",
    ];

    fn parse_start(extra: &[&str]) -> Result<ValidatorCommand, clap::Error> {
        ValidatorCommand::try_parse_from(
            BASE_START_ARGS.iter().copied().chain(extra.iter().copied()),
        )
    }

    #[test]
    fn encryption_key_defaults_to_insecure_hex() {
        let command = parse_start(&[]).expect("start without encryption options must parse");
        let ValidatorCommand::Start {
            encryption_key,
            encryption_key_kms_ciphertext,
            ..
        } = command
        else {
            panic!("expected the start command");
        };
        assert_eq!(encryption_key, INSECURE_ENCRYPTION_KEY_HEX);
        assert_eq!(encryption_key_kms_ciphertext, None);
    }

    #[test]
    fn encryption_key_kms_ciphertext_parses_alone() {
        let command = parse_start(&["--encryption-key.kms-ciphertext", "deadbeef"])
            .expect("KMS ciphertext without a hex key must parse");
        let ValidatorCommand::Start { encryption_key_kms_ciphertext, .. } = command else {
            panic!("expected the start command");
        };
        assert_eq!(encryption_key_kms_ciphertext.as_deref(), Some("deadbeef"));
    }

    #[test]
    fn encryption_key_hex_and_kms_ciphertext_conflict() {
        let result = parse_start(&[
            "--encryption-key.hex",
            INSECURE_ENCRYPTION_KEY_HEX,
            "--encryption-key.kms-ciphertext",
            "deadbeef",
        ]);
        let Err(error) = result else {
            panic!("hex key and KMS ciphertext together must be rejected");
        };
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }
}
