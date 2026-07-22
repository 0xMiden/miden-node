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
const ENV_NEXT_ENCRYPTION_KEY: &str = "MIDEN_VALIDATOR_NEXT_ENCRYPTION_KEY";
const ENV_NEXT_ENCRYPTION_KEY_ROTATION_BLOCK: &str =
    "MIDEN_VALIDATOR_NEXT_ENCRYPTION_KEY_ROTATION_BLOCK";
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

        /// Hex-encoded shared secret of the next transaction encryption key, scheduling a key
        /// rotation at the block given by `encryption-key.next.rotation-block`.
        ///
        /// Like the current key, this value and the rotation block must be identical across every
        /// validator in the set, and every validator must be reconfigured before the rotation
        /// block is reached. Must differ from the current encryption key.
        ///
        /// Requires `encryption-key.next.rotation-block`.
        #[arg(
            long = "encryption-key.next.hex",
            env = ENV_NEXT_ENCRYPTION_KEY,
            value_name = "VALIDATOR_NEXT_ENCRYPTION_KEY",
            requires = "encryption_key_rotation_block"
        )]
        encryption_key_next: Option<String>,

        /// Block number at which the next transaction encryption key replaces the current one.
        ///
        /// Requires `encryption-key.next.hex`.
        #[arg(
            long = "encryption-key.next.rotation-block",
            env = ENV_NEXT_ENCRYPTION_KEY_ROTATION_BLOCK,
            value_name = "ROTATION_BLOCK_NUM",
            requires = "encryption_key_next"
        )]
        encryption_key_rotation_block: Option<u32>,
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
                encryption_key,
                encryption_key_kms_ciphertext,
                encryption_key_next,
                encryption_key_rotation_block,
                ..
            } => {
                let address = listen;

                let encryption_key_hex = if let Some(ciphertext) = encryption_key_kms_ciphertext {
                    let ciphertext =
                        base64::engine::general_purpose::STANDARD
                            .decode(ciphertext)
                            .context("failed to decode the encryption key KMS ciphertext base64")?;
                    let encryption_key_bytes = miden_validator::decrypt_key_material(ciphertext)
                        .await
                        .context("failed to decrypt the encryption key with KMS")?;
                    hex::encode(encryption_key_bytes)
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

                    encryption_key
                };
                let decrypter: Arc<dyn TransactionInputDecrypter> = Arc::new(build_decrypter(
                    &encryption_key_hex,
                    encryption_key_next.as_deref(),
                    encryption_key_rotation_block,
                )?);

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

// TRANSACTION INPUT DECRYPTER CONSTRUCTION
// ================================================================================================

/// Builds the transaction input decrypter from the hex-encoded shared secret and, when a rotation
/// is scheduled, the hex-encoded next shared secret and its rotation block.
fn build_decrypter(
    encryption_key_hex: &str,
    next_key_hex: Option<&str>,
    rotation_block: Option<u32>,
) -> anyhow::Result<LocalX25519TransactionInputDecrypter> {
    let encryption_key_bytes =
        hex::decode(encryption_key_hex).context("failed to decode the encryption key hex")?;
    let encryption_key = KeyExchangeKey::read_from_bytes(&encryption_key_bytes)
        .context("failed to construct the encryption key")?;

    let mut decrypter = LocalX25519TransactionInputDecrypter::new(encryption_key);
    if let Some(next_key_hex) = next_key_hex {
        let rotation_block =
            rotation_block.context("encryption-key.next.hex requires a rotation block")?;
        let next_key_bytes =
            hex::decode(next_key_hex).context("failed to decode the next encryption key hex")?;
        if next_key_bytes == encryption_key_bytes {
            anyhow::bail!("the next encryption key must differ from the current encryption key");
        }
        let next_key = KeyExchangeKey::read_from_bytes(&next_key_bytes)
            .context("failed to construct the next encryption key")?;
        decrypter = decrypter.with_next_key(next_key, rotation_block);
        tracing::info!(
            target: LOG_TARGET,
            rotation_block,
            "Transaction encryption key rotation scheduled"
        );
    }
    Ok(decrypter)
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

    const NEXT_KEY_HEX: &str = "0303030303030303030303030303030303030303030303030303030303030303";

    /// The minimal `start` argument list the rotation flags are appended to.
    fn start_args() -> Vec<&'static str> {
        vec![
            "miden-validator",
            "start",
            "--listen",
            "127.0.0.1:0",
            "--data-directory",
            "/tmp/validator-data",
        ]
    }

    /// A rotation is only accepted as a complete pair: the next key without a rotation block and
    /// the rotation block without a next key must both be rejected at argument parsing.
    #[test]
    fn rotation_flags_require_each_other() {
        let mut next_only = start_args();
        next_only.extend(["--encryption-key.next.hex", NEXT_KEY_HEX]);
        assert!(ValidatorCommand::try_parse_from(next_only).is_err());

        let mut block_only = start_args();
        block_only.extend(["--encryption-key.next.rotation-block", "100"]);
        assert!(ValidatorCommand::try_parse_from(block_only).is_err());

        let mut both = start_args();
        both.extend([
            "--encryption-key.next.hex",
            NEXT_KEY_HEX,
            "--encryption-key.next.rotation-block",
            "100",
        ]);
        assert!(ValidatorCommand::try_parse_from(both).is_ok());

        assert!(ValidatorCommand::try_parse_from(start_args()).is_ok());
    }

    /// A scheduled rotation yields a decrypter announcing the next key at the rotation block.
    #[tokio::test]
    async fn build_decrypter_schedules_rotation() {
        let decrypter =
            build_decrypter(INSECURE_ENCRYPTION_KEY_HEX, Some(NEXT_KEY_HEX), Some(42)).unwrap();
        let info = decrypter.encryption_key().await.unwrap();
        let next = info.next_key.expect("rotation must be scheduled");
        assert_eq!(next.rotation_block_num, 42);

        let plain = build_decrypter(INSECURE_ENCRYPTION_KEY_HEX, None, None).unwrap();
        assert!(plain.encryption_key().await.unwrap().next_key.is_none());
    }

    /// Invalid rotation configurations must be rejected: a next key equal to the current one,
    /// undecodable hex, key material of the wrong width, and a missing rotation block.
    #[test]
    fn build_decrypter_rejects_invalid_rotation_config() {
        let same_key = build_decrypter(
            INSECURE_ENCRYPTION_KEY_HEX,
            Some(INSECURE_ENCRYPTION_KEY_HEX),
            Some(42),
        );
        assert!(same_key.err().unwrap().to_string().contains("must differ"));

        let bad_hex = build_decrypter(INSECURE_ENCRYPTION_KEY_HEX, Some("not hex"), Some(42));
        assert!(bad_hex.err().unwrap().to_string().contains("decode"));

        let short_key = build_decrypter(INSECURE_ENCRYPTION_KEY_HEX, Some("0badf00d"), Some(42));
        assert!(short_key.is_err());

        let missing_block = build_decrypter(INSECURE_ENCRYPTION_KEY_HEX, Some(NEXT_KEY_HEX), None);
        assert!(missing_block.err().unwrap().to_string().contains("rotation block"));
    }
}
