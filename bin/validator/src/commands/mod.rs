mod bootstrap;
mod genesis;
mod issue_private_record_share;
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
    EncodedGoldenOperatorKey,
    GoldenOperatorKey,
    LOG_TARGET,
    LocalX25519TransactionInputDecrypter,
    PrivateRecordSealer,
    StorageKeyEpoch,
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
const ENV_STORAGE_KEY_EPOCH: &str = "MIDEN_VALIDATOR_STORAGE_KEY_EPOCH";
const ENV_STORAGE_KEY_PUBLIC_SET: &str = "MIDEN_VALIDATOR_STORAGE_KEY_PUBLIC_SET";
const ENV_STORAGE_KEY_SECRET_SHARE: &str = "MIDEN_VALIDATOR_STORAGE_KEY_SECRET_SHARE";
const ENV_STORAGE_KEY_SETUP_CONTEXT: &str = "MIDEN_VALIDATOR_STORAGE_KEY_SETUP_CONTEXT";

/// A predefined, insecure shared transaction encryption key for development purposes.
pub(crate) const INSECURE_ENCRYPTION_KEY_HEX: &str =
    "0202020202020202020202020202020202020202020202020202020202020202";

// VALIDATOR COMMAND
// ================================================================================================

/// Local inputs for issuing one Golden private-record share.
#[derive(clap::Args)]
pub struct PrivateRecordShareOptions {
    /// Directory containing the validator database that owns the record.
    #[arg(long, env = ENV_DATA_DIRECTORY, value_name = "DIR")]
    data_directory: PathBuf,

    /// Hex-encoded private record identifier.
    #[arg(long, value_name = "RECORD_ID")]
    record_id: String,

    /// File that receives the canonical share bytes. Writes to stdout when omitted.
    #[arg(long, value_name = "FILE")]
    output: Option<PathBuf>,

    /// Canonical Golden storage key material for this validator.
    #[command(flatten)]
    storage_key: ValidatorStorageKey,
}

#[derive(Parser)]
#[command(version, about, long_about = None)]
pub enum ValidatorCommand {
    /// Builds the genesis block from a genesis configuration.
    ///
    /// Creates accounts from the genesis configuration, builds the genesis block, and writes the
    /// block and account secret files to disk.
    ///
    /// The genesis block is the chain's trust root and is not signed: its header commits to the
    /// full validator set — the `validators` public keys, which a genesis configuration file must
    /// list explicitly; only the built-in development configuration (used when `--config` is
    /// omitted) falls back to the predefined, insecure development key — and that set is required
    /// to sign every block after genesis. Building the genesis block needs no signing access to
    /// any validator's key, so one operator — who need not be a validator — runs this once and
    /// distributes the genesis block file.
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

    /// Issues this validator's Golden decryption share for one stored private record.
    IssuePrivateRecordShare(PrivateRecordShareOptions),

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

        /// Canonical Golden storage key material provisioned after setup.
        #[command(flatten)]
        storage_key: ValidatorStorageKey,
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
            Self::IssuePrivateRecordShare(options) => {
                issue_private_record_share::issue_from_options(options).await
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
                storage_key,
                ..
            } => {
                let address = listen;
                let private_record_sealer =
                    PrivateRecordSealer::from_operator_key(&storage_key.load()?);
                tracing::info!(
                    target: miden_validator::LOG_TARGET,
                    {
                        service.name = "miden-validator",
                        service.version = env!("CARGO_PKG_VERSION"),
                        validator.listen = %address,
                        data.directory = %data_directory.display(),
                        validator.signer = if signing_key_kms_id.is_some() { "kms" } else { "local" },
                        sqlite.connection_pool_size = sqlite_connection_pool_size.get(),
                    },
                    "Starting validator",
                );

                let decrypter =
                    resolve_decrypter(encryption_key, encryption_key_kms_ciphertext).await?;

                let signer =
                    ValidatorSigningKey { signing_key, signing_key_kms_id }.into_signer().await?;

                start::start(
                    address,
                    grpc_options,
                    start::ValidatorKeys { signer, decrypter, private_record_sealer },
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
            | Self::IssuePrivateRecordShare(_)
            | Self::Migrate { .. } => OpenTelemetry::Disabled,
        }
    }
}

/// Builds the transaction input decrypter from the configured shared encryption key: either the
/// KMS-wrapped ciphertext, or the hex-encoded key material.
async fn resolve_decrypter(
    encryption_key: String,
    encryption_key_kms_ciphertext: Option<String>,
) -> anyhow::Result<Arc<dyn TransactionInputDecrypter>> {
    let encryption_key_bytes = if let Some(ciphertext) = encryption_key_kms_ciphertext {
        let ciphertext = base64::engine::general_purpose::STANDARD
            .decode(ciphertext)
            .context("failed to decode the encryption key KMS ciphertext base64")?;
        miden_validator::decrypt_key_material(ciphertext)
            .await
            .context("failed to decrypt the encryption key with KMS")?
    } else {
        // Unlike the signing key, whose insecure default is caught at startup against the chain's
        // committed validator key, nothing cross-checks the encryption key. Warn loudly so the
        // default never runs in production unnoticed.
        if encryption_key == INSECURE_ENCRYPTION_KEY_HEX {
            tracing::warn!(
                target: LOG_TARGET,
                "Using the predefined, insecure transaction encryption key, configure \
                 --encryption-key.hex or --encryption-key.kms-ciphertext for production \
                 deployments"
            );
        }

        hex::decode(encryption_key).context("failed to decode the encryption key hex")?
    };
    let encryption_key = KeyExchangeKey::read_from_bytes(&encryption_key_bytes)
        .context("failed to construct the encryption key")?;
    Ok(Arc::new(LocalX25519TransactionInputDecrypter::new(encryption_key)))
}

/// Canonical Golden files needed to restore one validator storage key share.
#[derive(clap::Args)]
pub struct ValidatorStorageKey {
    /// Hex-encoded 32-byte storage key epoch.
    #[arg(
        long = "storage-key.epoch",
        env = ENV_STORAGE_KEY_EPOCH,
        value_name = "STORAGE_KEY_EPOCH"
    )]
    key_epoch: String,
    /// File containing canonical Golden `SetupContext` bytes.
    #[arg(
        long = "storage-key.setup-context",
        env = ENV_STORAGE_KEY_SETUP_CONTEXT,
        value_name = "FILE"
    )]
    setup_context: PathBuf,
    /// File containing canonical Golden `PublicKeySet` bytes.
    #[arg(
        long = "storage-key.public-key-set",
        env = ENV_STORAGE_KEY_PUBLIC_SET,
        value_name = "FILE"
    )]
    public_key_set: PathBuf,
    /// File containing this operator's canonical Golden `SecretShare` bytes.
    #[arg(
        long = "storage-key.secret-share",
        env = ENV_STORAGE_KEY_SECRET_SHARE,
        value_name = "FILE"
    )]
    secret_share: PathBuf,
}

impl ValidatorStorageKey {
    fn load(self) -> anyhow::Result<GoldenOperatorKey> {
        let key_epoch =
            hex::decode(self.key_epoch).context("failed to decode storage key epoch")?;
        let key_epoch = key_epoch.try_into().map_err(|bytes: Vec<u8>| {
            anyhow::anyhow!("storage key epoch has {} bytes, expected 32", bytes.len())
        })?;
        let operator_key = EncodedGoldenOperatorKey::new(
            StorageKeyEpoch::new(key_epoch),
            fs_err::read(&self.setup_context).with_context(|| {
                format!(
                    "failed to read storage key setup context from {}",
                    self.setup_context.display()
                )
            })?,
            fs_err::read(&self.public_key_set).with_context(|| {
                format!(
                    "failed to read storage key public key set from {}",
                    self.public_key_set.display()
                )
            })?,
            fs_err::read(&self.secret_share).with_context(|| {
                format!(
                    "failed to read storage key secret share from {}",
                    self.secret_share.display()
                )
            })?,
        )
        .decode()
        .context("failed to validate Golden storage key material")?;
        Ok(operator_key)
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
    use std::collections::BTreeMap;
    use std::path::Path;

    use golden_core::{GoldenGroup, GoldenScalar, ParticipantIndex, SessionId};
    use golden_ehtdh1::wire::{from_wire_bytes, to_wire_bytes};
    use golden_ehtdh1::{
        DecryptionShare,
        PublicKeySet,
        PublicShare,
        SecretShare,
        SetupContext,
        derive_context_session_id,
    };
    use golden_halo2curves::golden_group::{Secp256k1GoldenGroup, Secp256k1Scalar};
    use miden_protocol::Word;
    use miden_protocol::account::auth::AuthScheme;
    use miden_protocol::transaction::{TransactionId, TransactionInputs};
    use miden_protocol::utils::serde::{Deserializable, Serializable};
    use miden_testing::{Auth, MockChainBuilder};
    use rand_chacha_03::ChaCha20Rng;
    use rand_chacha_03::rand_core::SeedableRng;

    use super::*;

    type TestStorageGroup = Secp256k1GoldenGroup;

    const BASE_START_ARGS: [&str; 6] = [
        "miden-validator",
        "start",
        "--listen",
        "127.0.0.1:50101",
        "--data-directory",
        "/tmp/validator-data",
    ];
    const STORAGE_KEY_ARGS: [&str; 8] = [
        "--storage-key.epoch",
        "0909090909090909090909090909090909090909090909090909090909090909",
        "--storage-key.setup-context",
        "/tmp/setup.bin",
        "--storage-key.public-key-set",
        "/tmp/public.bin",
        "--storage-key.secret-share",
        "/tmp/secret.bin",
    ];

    fn parse_start(extra: &[&str]) -> Result<ValidatorCommand, clap::Error> {
        ValidatorCommand::try_parse_from(
            BASE_START_ARGS
                .iter()
                .copied()
                .chain(extra.iter().copied())
                .chain(STORAGE_KEY_ARGS.iter().copied()),
        )
    }

    fn test_operator_keys(epoch: u8, setup_marker: u8) -> Vec<GoldenOperatorKey> {
        let participants = [1, 2, 3].map(|value| ParticipantIndex::new(value).unwrap());
        let decryption_secret = Secp256k1Scalar::from_u64(11).unwrap();
        let decryption_coefficient = Secp256k1Scalar::from_u64(7).unwrap();
        let context_coefficient = Secp256k1Scalar::from_u64(13).unwrap();
        let mut public_shares = BTreeMap::new();
        let secret_shares = participants.map(|participant| {
            let point = participant.to_scalar::<Secp256k1Scalar>().unwrap();
            let decryption = decryption_secret.add(&decryption_coefficient.mul(&point));
            let context = context_coefficient.mul(&point);
            public_shares.insert(
                participant,
                PublicShare {
                    decryption: TestStorageGroup::mul_generator(&decryption),
                    context: TestStorageGroup::mul_generator(&context),
                },
            );
            SecretShare { participant, decryption, context }
        });
        let public_key_set = PublicKeySet::new(
            2,
            TestStorageGroup::mul_generator(&decryption_secret),
            public_shares,
        )
        .unwrap();
        let decryption_session_id = SessionId([2; 32]);
        let setup_context = SetupContext {
            backend_id: TestStorageGroup::BACKEND_ID.to_owned(),
            threshold: 2,
            registry_root: [1; 32],
            participants: participants.to_vec(),
            decryption_session_id,
            context_session_id: derive_context_session_id(decryption_session_id),
            decryption_transcript_root: [setup_marker; 32],
            context_transcript_root: [4; 32],
            epoch: [epoch; 32],
        };
        secret_shares
            .into_iter()
            .map(|secret_share| {
                GoldenOperatorKey::new(
                    StorageKeyEpoch::new([epoch; 32]),
                    setup_context.clone(),
                    public_key_set.clone(),
                    secret_share,
                )
                .unwrap()
            })
            .collect()
    }

    fn transaction_inputs() -> TransactionInputs {
        let mut builder = MockChainBuilder::new();
        let account = builder
            .add_existing_wallet(Auth::BasicAuth {
                auth_scheme: AuthScheme::Falcon512Poseidon2,
            })
            .unwrap();
        builder.build().unwrap().get_transaction_inputs(&account, &[], &[]).unwrap()
    }

    async fn issue_error(
        data_directory: &Path,
        encoded_record_id: &str,
        output: &Path,
        operator_key: &GoldenOperatorKey,
    ) -> String {
        let error = issue_private_record_share::issue(
            data_directory.to_path_buf(),
            encoded_record_id,
            Some(output),
            operator_key,
        )
        .await
        .unwrap_err();
        format!("{error:#}")
    }

    async fn issue_share_file(
        data_directory: &Path,
        encoded_record_id: &str,
        output: &Path,
        operator_key: &GoldenOperatorKey,
    ) -> Vec<u8> {
        issue_private_record_share::issue(
            data_directory.to_path_buf(),
            encoded_record_id,
            Some(output),
            operator_key,
        )
        .await
        .unwrap();
        fs_err::read(output).unwrap()
    }

    async fn store_private_record(
        database: &miden_node_db::sqlite::Database,
        record: miden_validator::StoredPrivateRecord,
    ) {
        database
            .write("insert_private_record", move |tx| {
                miden_validator::db::insert_private_record(tx, &record)
            })
            .await
            .unwrap();
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

    #[test]
    fn storage_key_is_required() {
        let Err(error) = ValidatorCommand::try_parse_from(BASE_START_ARGS) else {
            panic!("start without a storage key must fail");
        };
        assert_eq!(error.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn partial_storage_key_configuration_fails() {
        let Err(error) = ValidatorCommand::try_parse_from(BASE_START_ARGS.into_iter().chain([
            "--storage-key.epoch",
            "0909090909090909090909090909090909090909090909090909090909090909",
        ])) else {
            panic!("a partial storage key must fail");
        };
        assert_eq!(error.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn private_record_share_command_parses() {
        let command = ValidatorCommand::try_parse_from([
            "miden-validator",
            "issue-private-record-share",
            "--data-directory",
            "/tmp/validator-data",
            "--record-id",
            "0101010101010101010101010101010101010101010101010101010101010101",
            "--output",
            "/tmp/share.bin",
            "--storage-key.epoch",
            "0909090909090909090909090909090909090909090909090909090909090909",
            "--storage-key.setup-context",
            "/tmp/setup.bin",
            "--storage-key.public-key-set",
            "/tmp/public.bin",
            "--storage-key.secret-share",
            "/tmp/secret.bin",
        ])
        .expect("the local share command must parse");

        let ValidatorCommand::IssuePrivateRecordShare(PrivateRecordShareOptions {
            data_directory,
            record_id,
            output,
            ..
        }) = command
        else {
            panic!("expected the private record share command");
        };
        assert_eq!(data_directory, PathBuf::from("/tmp/validator-data"));
        assert_eq!(record_id, "0101010101010101010101010101010101010101010101010101010101010101",);
        assert_eq!(output, Some(PathBuf::from("/tmp/share.bin")));
    }

    #[tokio::test]
    async fn private_record_share_command_opens_stored_inputs() {
        let directory = tempfile::tempdir().unwrap();
        let database = miden_validator::db::setup(directory.path().join("validator.sqlite3"))
            .await
            .unwrap();
        let operator_keys = test_operator_keys(9, 3);
        let transaction_id = TransactionId::from_raw(Word::from([1u32, 2, 3, 4]));
        let record_id = miden_validator::PrivateRecordId::for_transaction_inputs(transaction_id);
        let context = miden_validator::PrivateRecordContext::new(
            miden_validator::PrivateRecordChainId::new([5; 32]),
            operator_keys[0].key_epoch(),
            record_id,
            transaction_id,
        );
        let inputs = transaction_inputs();
        let mut rng = ChaCha20Rng::from_seed([6; 32]);
        let record = miden_validator::PrivateRecordSealer::from_operator_key(&operator_keys[0])
            .seal(&mut rng, context, &inputs.to_bytes())
            .unwrap();
        let stored = record.clone();
        store_private_record(&database, record).await;

        let first_output = directory.path().join("share-1.bin");
        let second_output = directory.path().join("share-2.bin");
        let encoded_record_id = hex::encode(record_id.as_bytes());
        let first_share = issue_share_file(
            directory.path(),
            &encoded_record_id,
            &first_output,
            &operator_keys[0],
        )
        .await;
        let second_share = issue_share_file(
            directory.path(),
            &encoded_record_id,
            &second_output,
            &operator_keys[1],
        )
        .await;
        let shares = [first_share, second_share];
        for share in &shares {
            let decoded = from_wire_bytes::<DecryptionShare<TestStorageGroup>>(share).unwrap();
            assert_eq!(to_wire_bytes(&decoded), *share);
        }
        let request = miden_validator::PrivateRecordShareRequest::for_record(&stored);
        let opened = miden_validator::PrivateRecordCombiner::from_operator_key(&operator_keys[2])
            .unwrap()
            .open(&request, &stored, &shares)
            .unwrap();
        assert_eq!(TransactionInputs::read_from_bytes(&opened).unwrap(), inputs);

        assert!(
            issue_error(directory.path(), &"ff".repeat(32), &first_output, &operator_keys[0])
                .await
                .contains("was not found"),
        );

        let wrong_epoch = test_operator_keys(10, 3).remove(0);
        assert!(
            issue_error(directory.path(), &encoded_record_id, &first_output, &wrong_epoch)
                .await
                .contains("key epoch does not match"),
        );

        let wrong_setup = test_operator_keys(9, 7).remove(0);
        assert!(
            issue_error(directory.path(), &encoded_record_id, &first_output, &wrong_setup)
                .await
                .contains("setup does not match"),
        );

        let id = transaction_id.to_bytes();
        database
            .write("damage_private_record_context", move |tx| {
                tx.execute(
                    "UPDATE private_records SET chain_id = ?1 WHERE record_id = ?2",
                    &[&vec![8u8; 32], &id],
                )
            })
            .await
            .unwrap();
        assert!(
            issue_error(directory.path(), &encoded_record_id, &first_output, &operator_keys[0])
                .await
                .contains("invalid Golden content key ciphertext"),
        );

        let id = transaction_id.to_bytes();
        database
            .write("damage_private_record_encoding", move |tx| {
                tx.execute(
                    "UPDATE private_records \
                     SET chain_id = ?1, wrapped_content_key = ?2 \
                     WHERE record_id = ?3",
                    &[&vec![5u8; 32], &vec![0u8], &id],
                )
            })
            .await
            .unwrap();
        assert!(
            issue_error(directory.path(), &encoded_record_id, &first_output, &operator_keys[0])
                .await
                .contains("invalid Golden content key ciphertext"),
        );
    }
}
