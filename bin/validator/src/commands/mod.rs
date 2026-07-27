mod bootstrap;
mod start;

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use base64::Engine;
use clap::Parser;
use miden_node_utils::clap::GrpcOptionsInternal;
use miden_node_utils::logging::OpenTelemetry;
use miden_node_utils::shutdown::CancellationToken;
use miden_protocol::block::BlockNumber;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey;
use miden_protocol::crypto::dsa::eddsa_25519_sha512::KeyExchangeKey;
use miden_protocol::utils::serde::Deserializable;
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
const ENV_ENCRYPTION_KEY_ACTIVATION_BLOCK: &str = "MIDEN_VALIDATOR_ENCRYPTION_KEY_ACTIVATION_BLOCK";
const ENV_PREVIOUS_ENCRYPTION_KEY: &str = "MIDEN_VALIDATOR_PREVIOUS_ENCRYPTION_KEY";
const ENV_PREVIOUS_ENCRYPTION_KEY_KMS_CIPHERTEXT: &str =
    "MIDEN_VALIDATOR_PREVIOUS_ENCRYPTION_KEY_KMS_CIPHERTEXT";
const ENV_PREVIOUS_ENCRYPTION_KEY_ACTIVATION_BLOCK: &str =
    "MIDEN_VALIDATOR_PREVIOUS_ENCRYPTION_KEY_ACTIVATION_BLOCK";
const ENV_NEXT_ENCRYPTION_KEY: &str = "MIDEN_VALIDATOR_NEXT_ENCRYPTION_KEY";
const ENV_NEXT_ENCRYPTION_KEY_KMS_CIPHERTEXT: &str =
    "MIDEN_VALIDATOR_NEXT_ENCRYPTION_KEY_KMS_CIPHERTEXT";
const ENV_NEXT_ENCRYPTION_KEY_ACTIVATION_BLOCK: &str =
    "MIDEN_VALIDATOR_NEXT_ENCRYPTION_KEY_ACTIVATION_BLOCK";
const ENV_GENESIS_CONFIG_FILE: &str = "MIDEN_VALIDATOR_GENESIS_CONFIG_FILE";
const ENV_SQLITE_CONNECTION_POOL_SIZE: &str = "MIDEN_VALIDATOR_SQLITE_CONNECTION_POOL_SIZE";

/// A predefined, insecure validator signing key for development purposes.
pub(crate) const INSECURE_SIGNING_KEY_HEX: &str =
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
        /// Configuration for the validator signing key used to sign the genesis block.
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
            default_value = INSECURE_SIGNING_KEY_HEX,
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

        /// Manual transaction encryption key schedule.
        #[command(flatten)]
        encryption_keys: ValidatorEncryptionKeys,
    },
}

#[derive(clap::Args)]
pub(crate) struct ValidatorEncryptionKeys {
    /// Hex-encoded shared current transaction encryption secret key.
    ///
    /// Unlike the per-validator signing key, this value must be identical across every validator
    /// in the set. If not provided, a predefined insecure key is used.
    #[arg(
        long = "encryption-key.hex",
        env = ENV_ENCRYPTION_KEY,
        value_name = "VALIDATOR_ENCRYPTION_KEY",
        default_value = INSECURE_ENCRYPTION_KEY_HEX,
        group = "encryption_key_source"
    )]
    current_key: String,

    /// Base64-encoded KMS ciphertext of the current transaction encryption secret key.
    #[arg(
        long = "encryption-key.kms-ciphertext",
        env = ENV_ENCRYPTION_KEY_KMS_CIPHERTEXT,
        value_name = "VALIDATOR_ENCRYPTION_KEY_KMS_CIPHERTEXT",
        group = "encryption_key_source"
    )]
    current_key_kms_ciphertext: Option<String>,

    /// Epoch-boundary block at which the current key became active.
    #[arg(
        long = "encryption-key.activation-block",
        env = ENV_ENCRYPTION_KEY_ACTIVATION_BLOCK,
        value_name = "BLOCK_NUM",
        default_value_t = 0
    )]
    current_key_activation_block: u32,

    /// Hex-encoded previous transaction encryption secret key retained for grace decryption.
    #[arg(
        long = "encryption-key.previous.hex",
        env = ENV_PREVIOUS_ENCRYPTION_KEY,
        value_name = "PREVIOUS_VALIDATOR_ENCRYPTION_KEY",
        group = "previous_encryption_key_source",
        requires = "previous_key_activation_block"
    )]
    previous_key: Option<String>,

    /// Base64-encoded KMS ciphertext of the previous transaction encryption secret key.
    #[arg(
        long = "encryption-key.previous.kms-ciphertext",
        env = ENV_PREVIOUS_ENCRYPTION_KEY_KMS_CIPHERTEXT,
        value_name = "PREVIOUS_VALIDATOR_ENCRYPTION_KEY_KMS_CIPHERTEXT",
        group = "previous_encryption_key_source",
        requires = "previous_key_activation_block"
    )]
    previous_key_kms_ciphertext: Option<String>,

    /// Epoch-boundary block at which the previous key became active.
    #[arg(
        long = "encryption-key.previous.activation-block",
        env = ENV_PREVIOUS_ENCRYPTION_KEY_ACTIVATION_BLOCK,
        value_name = "BLOCK_NUM",
        requires = "previous_encryption_key_source"
    )]
    previous_key_activation_block: Option<u32>,

    /// Hex-encoded next transaction encryption secret key.
    #[arg(
        long = "encryption-key.next.hex",
        env = ENV_NEXT_ENCRYPTION_KEY,
        value_name = "NEXT_VALIDATOR_ENCRYPTION_KEY",
        group = "next_encryption_key_source",
        requires = "next_key_activation_block"
    )]
    next_key: Option<String>,

    /// Base64-encoded KMS ciphertext of the next transaction encryption secret key.
    #[arg(
        long = "encryption-key.next.kms-ciphertext",
        env = ENV_NEXT_ENCRYPTION_KEY_KMS_CIPHERTEXT,
        value_name = "NEXT_VALIDATOR_ENCRYPTION_KEY_KMS_CIPHERTEXT",
        group = "next_encryption_key_source",
        requires = "next_key_activation_block"
    )]
    next_key_kms_ciphertext: Option<String>,

    /// Epoch-boundary block at which the next key will become active.
    #[arg(
        long = "encryption-key.next.activation-block",
        env = ENV_NEXT_ENCRYPTION_KEY_ACTIVATION_BLOCK,
        value_name = "BLOCK_NUM",
        requires = "next_encryption_key_source"
    )]
    next_key_activation_block: Option<u32>,
}

impl ValidatorEncryptionKeys {
    async fn into_decrypter(self) -> anyhow::Result<LocalX25519TransactionInputDecrypter> {
        if self.current_key_kms_ciphertext.is_none()
            && self.current_key == INSECURE_ENCRYPTION_KEY_HEX
        {
            tracing::warn!(
                target: LOG_TARGET,
                "Using the predefined, insecure transaction encryption key, configure \
                 --encryption-key.hex or --encryption-key.kms-ciphertext for production \
                 deployments"
            );
        }

        let current =
            load_encryption_key(Some(self.current_key), self.current_key_kms_ciphertext, "current")
                .await?
                .expect("the current encryption key always has a default");
        let previous =
            load_encryption_key(self.previous_key, self.previous_key_kms_ciphertext, "previous")
                .await?;
        let next = load_encryption_key(self.next_key, self.next_key_kms_ciphertext, "next").await?;

        let previous =
            pair_key_with_activation(previous, self.previous_key_activation_block, "previous")?;
        let next = pair_key_with_activation(next, self.next_key_activation_block, "next")?;

        LocalX25519TransactionInputDecrypter::from_schedule(
            previous,
            (current, BlockNumber::from(self.current_key_activation_block)),
            next,
        )
    }
}

async fn load_encryption_key(
    hex_key: Option<String>,
    kms_ciphertext: Option<String>,
    role: &str,
) -> anyhow::Result<Option<KeyExchangeKey>> {
    let key_bytes = if let Some(ciphertext) = kms_ciphertext {
        let ciphertext =
            base64::engine::general_purpose::STANDARD.decode(ciphertext).with_context(|| {
                format!("failed to decode the {role} encryption key KMS ciphertext")
            })?;
        Some(
            miden_validator::decrypt_key_material(ciphertext)
                .await
                .with_context(|| format!("failed to decrypt the {role} encryption key with KMS"))?,
        )
    } else {
        hex_key
            .map(|key| {
                hex::decode(key)
                    .with_context(|| format!("failed to decode the {role} encryption key hex"))
            })
            .transpose()?
    };

    key_bytes
        .map(|key| {
            KeyExchangeKey::read_from_bytes(&key)
                .with_context(|| format!("failed to parse the {role} transaction encryption key"))
        })
        .transpose()
}

fn pair_key_with_activation(
    key: Option<KeyExchangeKey>,
    activation_block: Option<u32>,
    role: &str,
) -> anyhow::Result<Option<(KeyExchangeKey, BlockNumber)>> {
    match (key, activation_block) {
        (Some(key), Some(activation_block)) => Ok(Some((key, BlockNumber::from(activation_block)))),
        (None, None) => Ok(None),
        (Some(_), None) => {
            anyhow::bail!("{role} encryption key requires an activation block")
        },
        (None, Some(_)) => {
            anyhow::bail!("{role} encryption key activation requires a key")
        },
    }
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
                signing_key,
            } => {
                bootstrap::bootstrap(
                    &genesis_block_directory,
                    &accounts_directory,
                    &data_directory,
                    sqlite_connection_pool_size,
                    genesis_config_file.as_ref(),
                    signing_key,
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
                signing_key,
                data_directory,
                signing_key_kms_id,
                sqlite_connection_pool_size,
                encryption_keys,
                ..
            } => {
                let address = listen;
                let decrypter: Arc<dyn TransactionInputDecrypter> =
                    Arc::new(encryption_keys.into_decrypter().await?);

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
            Self::Bootstrap { .. } | Self::Migrate { .. } => OpenTelemetry::Disabled,
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
        default_value = INSECURE_SIGNING_KEY_HEX,
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
    use miden_protocol::crypto::utils::Serializable;

    use super::*;

    const KEY_A: &str = "0303030303030303030303030303030303030303030303030303030303030303";
    const KEY_B: &str = "0404040404040404040404040404040404040404040404040404040404040404";
    const KEY_C: &str = "0505050505050505050505050505050505050505050505050505050505050505";

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
        let ValidatorCommand::Start { encryption_keys, .. } = command else {
            panic!("expected the start command");
        };
        assert_eq!(encryption_keys.current_key, INSECURE_ENCRYPTION_KEY_HEX);
        assert_eq!(encryption_keys.current_key_kms_ciphertext, None);
        assert_eq!(encryption_keys.current_key_activation_block, 0);
        assert!(encryption_keys.previous_key.is_none());
        assert!(encryption_keys.next_key.is_none());
    }

    #[test]
    fn encryption_key_kms_ciphertext_parses_alone() {
        let command = parse_start(&["--encryption-key.kms-ciphertext", "deadbeef"])
            .expect("KMS ciphertext without a hex key must parse");
        let ValidatorCommand::Start { encryption_keys, .. } = command else {
            panic!("expected the start command");
        };
        assert_eq!(encryption_keys.current_key_kms_ciphertext.as_deref(), Some("deadbeef"));
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

    #[tokio::test]
    async fn complete_manual_encryption_key_schedule_parses() {
        let command = parse_start(&[
            "--encryption-key.previous.hex",
            KEY_A,
            "--encryption-key.previous.activation-block",
            "0",
            "--encryption-key.hex",
            KEY_B,
            "--encryption-key.activation-block",
            "65536",
            "--encryption-key.next.hex",
            KEY_C,
            "--encryption-key.next.activation-block",
            "131072",
        ])
        .expect("a complete previous, current, and next schedule must parse");
        let ValidatorCommand::Start { encryption_keys, .. } = command else {
            panic!("expected the start command");
        };
        let provider = encryption_keys.into_decrypter().await.unwrap();
        let schedule = provider.encryption_key_schedule(BlockNumber::from_epoch(1)).await.unwrap();

        let current = KeyExchangeKey::read_from_bytes(&[4; 32]).unwrap();
        let next = KeyExchangeKey::read_from_bytes(&[5; 32]).unwrap();
        assert_eq!(schedule.current_key.key_id, current.public_key().to_commitment().to_bytes());
        assert_eq!(
            schedule.next_key.unwrap().key.key_id,
            next.public_key().to_commitment().to_bytes()
        );
    }

    #[test]
    fn scheduled_key_requires_an_activation_block() {
        let Err(error) = parse_start(&["--encryption-key.next.hex", KEY_C]) else {
            panic!("a next key without an activation must be rejected");
        };
        assert_eq!(error.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[tokio::test]
    async fn startup_rejects_non_boundary_key_activation() {
        let command = parse_start(&[
            "--encryption-key.previous.hex",
            KEY_A,
            "--encryption-key.previous.activation-block",
            "0",
            "--encryption-key.hex",
            KEY_B,
            "--encryption-key.activation-block",
            "65537",
        ])
        .unwrap();
        let ValidatorCommand::Start { encryption_keys, .. } = command else {
            panic!("expected the start command");
        };

        assert!(encryption_keys.into_decrypter().await.is_err());
    }
}
