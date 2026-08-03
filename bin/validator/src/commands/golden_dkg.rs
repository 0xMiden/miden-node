use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, ensure};
use golden_core::wire::to_wire_bytes;
use golden_core::{
    DkgConfig,
    GoldenGroup,
    GoldenScalar,
    ParticipantIndex,
    ParticipantRegistry,
    SessionId,
};
use golden_ehtdh1::derive_context_session_id;
use golden_halo2curves::golden_group::Secp256k1GoldenGroup;
use miden_node_store::genesis::GenesisBlock;
use miden_node_utils::genesis::read_genesis_block;
use miden_protocol::Word;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{PublicKey, Signature};
use miden_protocol::crypto::hash::rpo::Rpo256;
use miden_protocol::utils::serde::{Deserializable, Serializable};
use miden_validator::ValidatorSigner;
use rand_core_06::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::ValidatorSigningKey;

type StorageGroup = Secp256k1GoldenGroup;
type StorageScalar = <StorageGroup as GoldenGroup>::Scalar;

const REGISTRATION_VERSION: &str = "miden-golden-dkg-registration-v1";
const MANIFEST_VERSION: &str = "miden-golden-dkg-manifest-v1";
const REGISTRATION_SIGNATURE_DOMAIN: &[u8] = b"miden-golden-dkg-registration-signature-v1";
const IDENTITY_SECRET_MAGIC: &[u8] = b"miden-golden-dkg-identity-v1\0";
const IDENTITY_SECRET_FILE: &str = "identity-secret.wire";
const REGISTRATION_FILE: &str = "registration.toml";
const MANIFEST_FILE: &str = "manifest.toml";
const DECRYPTION_CONFIG_FILE: &str = "decryption-config.wire";
const CONTEXT_CONFIG_FILE: &str = "context-config.wire";

/// Inputs for one Golden DKG ceremony command.
#[derive(clap::Args)]
pub struct GoldenDkgOptions {
    #[command(subcommand)]
    command: GoldenDkgCommand,
}

/// Golden DKG ceremony commands.
#[derive(clap::Subcommand)]
enum GoldenDkgCommand {
    /// Generates this validator's DKG identity and public registration.
    Identity {
        /// Trusted genesis block for the network.
        #[arg(long, value_name = "FILE")]
        genesis: PathBuf,

        /// Validator signing key committed by genesis.
        #[command(flatten)]
        signing_key: ValidatorSigningKey,

        /// New directory that receives the identity and registration files.
        #[arg(long, value_name = "DIR")]
        output_directory: PathBuf,
    },

    /// Builds the public configurations for both Golden DKG rounds.
    Prepare {
        /// Trusted genesis block for the network.
        #[arg(long, value_name = "FILE")]
        genesis: PathBuf,

        /// Number of shares needed to decrypt a private record.
        #[arg(long, value_name = "NUM")]
        threshold: usize,

        /// Hex-encoded 32-byte storage-key epoch.
        #[arg(long, value_name = "HEX")]
        epoch: String,

        /// Public registration from one validator. Repeat once per genesis validator.
        #[arg(long, required = true, value_name = "FILE")]
        registration: Vec<PathBuf>,

        /// New directory that receives the manifest and public DKG configurations.
        #[arg(long, value_name = "DIR")]
        output_directory: PathBuf,
    },
}

#[derive(Debug, Deserialize, Serialize)]
struct Registration {
    version: String,
    genesis_commitment: String,
    validator_public_key: String,
    dkg_identity_public_key: String,
    validator_signature: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Manifest {
    version: String,
    genesis_commitment: String,
    threshold: usize,
    epoch: String,
    beta: String,
    decryption_session_id: String,
    context_session_id: String,
    decryption_config_sha256: String,
    context_config_sha256: String,
    participants: Vec<ManifestParticipant>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ManifestParticipant {
    participant_index: u32,
    validator_public_key: String,
    dkg_identity_public_key: String,
}

/// Runs one Golden DKG ceremony command.
pub async fn run(options: GoldenDkgOptions) -> anyhow::Result<()> {
    match options.command {
        GoldenDkgCommand::Identity { genesis, signing_key, output_directory } => {
            let signer = signing_key.into_signer().await?;
            generate_identity(&genesis, &signer, &output_directory).await
        },
        GoldenDkgCommand::Prepare {
            genesis,
            threshold,
            epoch,
            registration,
            output_directory,
        } => prepare(&genesis, threshold, &epoch, &registration, &output_directory),
    }
}

/// Generates one validator's private DKG identity and public registration.
async fn generate_identity(
    genesis_path: &Path,
    signer: &ValidatorSigner,
    output_directory: &Path,
) -> anyhow::Result<()> {
    let genesis = read_trusted_genesis(genesis_path)?;
    let genesis_commitment = genesis.inner().header().commitment();
    let validator_public_key = signer.public_key();
    ensure!(
        genesis
            .inner()
            .header()
            .validator_keys()
            .as_keys()
            .contains(&validator_public_key),
        "validator signing key is not committed by genesis",
    );
    let identity_secret = StorageScalar::random(&mut OsRng);
    ensure!(!bool::from(identity_secret.is_zero()), "generated a zero DKG identity secret");
    let identity_public_key = StorageGroup::mul_generator(&identity_secret);
    let signature_commitment = registration_signature_commitment(
        genesis_commitment,
        &validator_public_key,
        &identity_public_key,
    );
    let validator_signature = signer
        .sign_commitment(signature_commitment)
        .await
        .context("failed to sign DKG registration")?;

    let registration = Registration {
        version: REGISTRATION_VERSION.to_owned(),
        genesis_commitment: hex::encode(genesis_commitment.to_bytes()),
        validator_public_key: hex::encode(validator_public_key.to_bytes()),
        dkg_identity_public_key: hex::encode(StorageGroup::encode_element(&identity_public_key)),
        validator_signature: hex::encode(validator_signature.to_bytes()),
    };
    let registration =
        toml::to_string_pretty(&registration).context("failed to encode DKG registration")?;
    let secret = encode_identity_secret(&identity_secret);

    publish_directory(output_directory, |directory| {
        write_new_file(&directory.join(IDENTITY_SECRET_FILE), &secret, true)?;
        write_new_file(&directory.join(REGISTRATION_FILE), registration.as_bytes(), false)
    })?;

    println!("Golden DKG identity written to {}.", output_directory.display());
    Ok(())
}

/// Builds the genesis-bound manifest and public configurations for both DKG rounds.
fn prepare(
    genesis_path: &Path,
    threshold: usize,
    epoch: &str,
    registration_paths: &[PathBuf],
    output_directory: &Path,
) -> anyhow::Result<()> {
    let epoch = decode_fixed_hex::<32>(epoch, "storage-key epoch")?;
    let genesis = read_trusted_genesis(genesis_path)?;
    let genesis_commitment = genesis.inner().header().commitment();
    let validator_keys = genesis.inner().header().validator_keys().as_keys();

    ensure!(
        registration_paths.len() == validator_keys.len(),
        "expected {} registrations, got {}",
        validator_keys.len(),
        registration_paths.len(),
    );

    let mut registrations = read_validated_registrations(registration_paths, genesis_commitment)?;

    let mut registry_entries = Vec::with_capacity(validator_keys.len());
    let mut participants = Vec::with_capacity(validator_keys.len());
    for (offset, validator_key) in validator_keys.iter().enumerate() {
        let validator_key_hex = hex::encode(validator_key.to_bytes());
        let identity_key =
            registrations.remove(validator_key.to_bytes().as_slice()).with_context(|| {
                format!("missing registration for genesis validator {validator_key_hex}")
            })?;
        let participant = ParticipantIndex::new(
            u32::try_from(offset + 1).context("too many Golden DKG participants")?,
        )?;
        let identity_key_hex = hex::encode(StorageGroup::encode_element(&identity_key));

        registry_entries.push((participant, identity_key));
        participants.push(ManifestParticipant {
            participant_index: participant.get(),
            validator_public_key: validator_key_hex,
            dkg_identity_public_key: identity_key_hex,
        });
    }
    ensure!(
        registrations.is_empty(),
        "registration set contains a validator outside genesis"
    );

    let beta = StorageScalar::random(&mut OsRng);
    ensure!(!bool::from(beta.is_zero()), "generated a zero DKG beta");
    let decryption_session_id = SessionId::random(&mut OsRng);
    let context_session_id = derive_context_session_id(decryption_session_id);
    let registry: ParticipantRegistry<StorageGroup> = ParticipantRegistry::new(registry_entries)?;
    let decryption_config =
        DkgConfig::new(threshold, decryption_session_id, beta, registry.clone())?;
    let context_config = DkgConfig::new(threshold, context_session_id, beta, registry)?;
    let decryption_config = to_wire_bytes(&decryption_config);
    let context_config = to_wire_bytes(&context_config);

    let manifest = Manifest {
        version: MANIFEST_VERSION.to_owned(),
        genesis_commitment: hex::encode(genesis_commitment.to_bytes()),
        threshold,
        epoch: hex::encode(epoch),
        beta: hex::encode(beta.to_repr()),
        decryption_session_id: hex::encode(decryption_session_id.0),
        context_session_id: hex::encode(context_session_id.0),
        decryption_config_sha256: sha256_hex(&decryption_config),
        context_config_sha256: sha256_hex(&context_config),
        participants,
    };
    let manifest = toml::to_string_pretty(&manifest).context("failed to encode DKG manifest")?;

    publish_directory(output_directory, |directory| {
        write_new_file(&directory.join(MANIFEST_FILE), manifest.as_bytes(), false)?;
        write_new_file(&directory.join(DECRYPTION_CONFIG_FILE), &decryption_config, false)?;
        write_new_file(&directory.join(CONTEXT_CONFIG_FILE), &context_config, false)
    })?;

    println!("Golden DKG configuration written to {}.", output_directory.display());
    Ok(())
}

/// Reads and validates one public DKG registration.
fn read_registration(path: &Path) -> anyhow::Result<Registration> {
    let contents = fs_err::read_to_string(path)
        .with_context(|| format!("failed to read registration {}", path.display()))?;
    let registration: Registration = toml::from_str(&contents)
        .with_context(|| format!("failed to decode registration {}", path.display()))?;
    ensure!(
        registration.version == REGISTRATION_VERSION,
        "unsupported registration version in {}",
        path.display(),
    );
    Ok(registration)
}

/// Reads registrations and verifies their genesis binding and validator signatures.
fn read_validated_registrations(
    paths: &[PathBuf],
    genesis_commitment: Word,
) -> anyhow::Result<BTreeMap<Vec<u8>, <StorageGroup as GoldenGroup>::Element>> {
    let mut registrations = BTreeMap::new();
    let mut identity_keys = BTreeSet::new();
    for path in paths {
        let registration = read_registration(path)?;
        let validator_key = decode_validator_public_key(&registration.validator_public_key)?;
        let identity_key = decode_identity_public_key(&registration.dkg_identity_public_key)?;
        let signature = decode_validator_signature(&registration.validator_signature)?;

        ensure!(
            registration.genesis_commitment == hex::encode(genesis_commitment.to_bytes()),
            "registration in {} belongs to a different genesis block",
            path.display(),
        );
        ensure!(
            signature.verify(
                registration_signature_commitment(
                    genesis_commitment,
                    &validator_key,
                    &identity_key,
                ),
                &validator_key,
            ),
            "invalid validator signature in {}",
            path.display(),
        );
        ensure!(
            identity_keys.insert(StorageGroup::encode_element(&identity_key).as_ref().to_vec()),
            "duplicate DKG identity public key in {}",
            path.display(),
        );
        ensure!(
            registrations.insert(validator_key.to_bytes(), identity_key).is_none(),
            "duplicate validator registration in {}",
            path.display(),
        );
    }
    Ok(registrations)
}

/// Reads and validates the trusted genesis block used by the ceremony.
fn read_trusted_genesis(path: &Path) -> anyhow::Result<GenesisBlock> {
    GenesisBlock::try_from(read_genesis_block(path)?).context("failed to validate genesis block")
}

/// Commits a validator signature to one genesis-bound DKG identity registration.
fn registration_signature_commitment(
    genesis_commitment: Word,
    validator_public_key: &PublicKey,
    identity_public_key: &<StorageGroup as GoldenGroup>::Element,
) -> Word {
    let mut bytes = Vec::with_capacity(
        REGISTRATION_SIGNATURE_DOMAIN.len()
            + Word::SERIALIZED_SIZE
            + validator_public_key.to_bytes().len()
            + StorageGroup::ELEMENT_REPR_BYTES,
    );
    bytes.extend_from_slice(REGISTRATION_SIGNATURE_DOMAIN);
    bytes.extend_from_slice(&genesis_commitment.to_bytes());
    bytes.extend_from_slice(&validator_public_key.to_bytes());
    bytes.extend_from_slice(StorageGroup::encode_element(identity_public_key).as_ref());
    Rpo256::hash(&bytes)
}

/// Parses a validator public key and requires its canonical hex form.
fn decode_validator_public_key(value: &str) -> anyhow::Result<PublicKey> {
    let bytes = decode_hex(value, "validator public key")?;
    let public_key = PublicKey::read_from_bytes(&bytes).context("invalid validator public key")?;
    ensure!(public_key.to_bytes() == bytes, "non-canonical validator public key");
    Ok(public_key)
}

/// Parses a canonical validator registration signature.
fn decode_validator_signature(value: &str) -> anyhow::Result<Signature> {
    let bytes = decode_hex(value, "validator signature")?;
    let signature = Signature::read_from_bytes(&bytes).context("invalid validator signature")?;
    ensure!(signature.to_bytes() == bytes, "non-canonical validator signature");
    Ok(signature)
}

/// Parses a non-identity Golden DKG public key.
fn decode_identity_public_key(
    value: &str,
) -> anyhow::Result<<StorageGroup as GoldenGroup>::Element> {
    let bytes = decode_hex(value, "DKG identity public key")?;
    let repr = <StorageGroup as GoldenGroup>::ElementRepr::try_from(bytes)
        .map_err(|_| anyhow::anyhow!("invalid DKG identity public key length"))?;
    let public_key =
        StorageGroup::decode_element(&repr).context("invalid DKG identity public key")?;
    ensure!(
        !bool::from(StorageGroup::is_identity(&public_key)),
        "DKG identity public key is the identity"
    );
    Ok(public_key)
}

/// Encodes a private DKG identity with a fixed format marker.
fn encode_identity_secret(secret: &StorageScalar) -> Zeroizing<Vec<u8>> {
    let mut encoded =
        Zeroizing::new(Vec::with_capacity(IDENTITY_SECRET_MAGIC.len() + StorageScalar::REPR_BYTES));
    encoded.extend_from_slice(IDENTITY_SECRET_MAGIC);
    encoded.extend_from_slice(secret.to_repr().as_ref());
    encoded
}

/// Decodes a private DKG identity and rejects malformed or zero scalars.
#[cfg(test)]
fn decode_identity_secret(bytes: &[u8]) -> anyhow::Result<StorageScalar> {
    let scalar_bytes = bytes
        .strip_prefix(IDENTITY_SECRET_MAGIC)
        .context("invalid DKG identity secret format")?;
    ensure!(
        scalar_bytes.len() == StorageScalar::REPR_BYTES,
        "invalid DKG identity secret length",
    );
    let repr = <StorageScalar as GoldenScalar>::Repr::try_from(scalar_bytes.to_vec())
        .map_err(|_| anyhow::anyhow!("invalid DKG identity secret length"))?;
    let secret = StorageScalar::from_repr(&repr).context("invalid DKG identity secret")?;
    ensure!(!bool::from(secret.is_zero()), "DKG identity secret is zero");
    Ok(secret)
}

/// Publishes a complete set of ceremony files under a new directory.
fn publish_directory(
    output_directory: &Path,
    write: impl FnOnce(&Path) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    ensure!(!output_directory.exists(), "output directory already exists");
    let parent = output_directory.parent().unwrap_or_else(|| Path::new("."));
    fs_err::create_dir_all(parent).context("failed to create output parent directory")?;
    let temporary = tempfile::Builder::new()
        .prefix(".golden-dkg-")
        .tempdir_in(parent)
        .context("failed to create temporary output directory")?;
    write(temporary.path())?;
    fs_err::rename(temporary.path(), output_directory)
        .context("failed to publish output directory")?;
    Ok(())
}

/// Creates one ceremony file without replacing an existing file.
fn write_new_file(path: &Path, bytes: &[u8], private: bool) -> anyhow::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.sync_all().with_context(|| format!("failed to sync {}", path.display()))?;
    Ok(())
}

/// Parses canonical lowercase hex.
fn decode_hex(value: &str, name: &str) -> anyhow::Result<Vec<u8>> {
    ensure!(
        value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "{name} must use lowercase hex",
    );
    let bytes = hex::decode(value).with_context(|| format!("invalid {name}"))?;
    ensure!(hex::encode(&bytes) == value, "non-canonical {name}");
    Ok(bytes)
}

/// Parses a fixed-size canonical hex value.
fn decode_fixed_hex<const N: usize>(value: &str, name: &str) -> anyhow::Result<[u8; N]> {
    decode_hex(value, name)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("{name} must be {N} bytes"))
}

/// Returns the SHA-256 digest of one public ceremony artifact.
fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use golden_core::wire::from_wire_bytes;
    use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    struct TestGenesis {
        path: PathBuf,
        signing_keys: Vec<SigningKey>,
        validator_keys: Vec<PublicKey>,
    }

    /// Creates a genesis block for three validators.
    fn write_genesis(root: &Path) -> TestResultWith<TestGenesis> {
        let signing_keys = vec![SigningKey::new(), SigningKey::new(), SigningKey::new()];
        let config = format!(
            concat!(
                "version = 1\n",
                "timestamp = 1717344256\n",
                "validators = [\"{}\", \"{}\", \"{}\"]\n",
                "\n[fee_parameters]\n",
                "verification_base_fee = 0\n",
            ),
            hex::encode(signing_keys[0].public_key().to_bytes()),
            hex::encode(signing_keys[1].public_key().to_bytes()),
            hex::encode(signing_keys[2].public_key().to_bytes()),
        );
        let config_path = root.join("genesis.toml");
        fs_err::write(&config_path, config)?;
        let genesis_directory = root.join("genesis");
        let accounts_directory = root.join("accounts");
        super::super::genesis::generate(
            &genesis_directory,
            &accounts_directory,
            Some(&config_path),
        )?;
        let genesis =
            GenesisBlock::try_from(read_genesis_block(&genesis_directory.join("genesis.dat"))?)?;
        Ok(TestGenesis {
            path: genesis_directory.join("genesis.dat"),
            signing_keys,
            validator_keys: genesis.inner().header().validator_keys().as_keys().to_vec(),
        })
    }

    type TestResultWith<T> = Result<T, Box<dyn std::error::Error>>;

    #[tokio::test]
    async fn identity_round_trip_matches_public_registration() -> TestResult {
        let root = tempfile::tempdir()?;
        let genesis = write_genesis(root.path())?;
        let signing_key = genesis.signing_keys[0].clone();
        let validator_key = signing_key.public_key();
        let signer = ValidatorSigner::new_local(signing_key);
        let output = root.path().join("identity");

        generate_identity(&genesis.path, &signer, &output).await?;

        let registration = read_registration(&output.join(REGISTRATION_FILE))?;
        let secret_bytes = Zeroizing::new(fs_err::read(output.join(IDENTITY_SECRET_FILE))?);
        let secret = decode_identity_secret(&secret_bytes)?;
        let public_key = decode_identity_public_key(&registration.dkg_identity_public_key)?;
        let signature = decode_validator_signature(&registration.validator_signature)?;
        let genesis_commitment = read_trusted_genesis(&genesis.path)?.inner().header().commitment();
        assert_eq!(StorageGroup::mul_generator(&secret), public_key);
        assert_eq!(registration.validator_public_key, hex::encode(validator_key.to_bytes()));
        assert!(signature.verify(
            registration_signature_commitment(genesis_commitment, &validator_key, &public_key),
            &validator_key,
        ));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode =
                fs_err::metadata(output.join(IDENTITY_SECRET_FILE))?.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        Ok(())
    }

    #[test]
    fn identity_secret_rejects_malformed_input() {
        assert!(decode_identity_secret(IDENTITY_SECRET_MAGIC).is_err());
        let mut zero = IDENTITY_SECRET_MAGIC.to_vec();
        zero.extend_from_slice(&[0; StorageScalar::REPR_BYTES]);
        assert!(decode_identity_secret(&zero).is_err());
        zero.push(0);
        assert!(decode_identity_secret(&zero).is_err());
    }

    #[tokio::test]
    async fn prepare_binds_configs_to_canonical_genesis_order() -> TestResult {
        let root = tempfile::tempdir()?;
        let genesis = write_genesis(root.path())?;
        let mut registrations = Vec::new();
        for (position, signing_key) in genesis.signing_keys.iter().rev().enumerate() {
            let directory = root.path().join(format!("identity-{position}"));
            generate_identity(
                &genesis.path,
                &ValidatorSigner::new_local(signing_key.clone()),
                &directory,
            )
            .await?;
            registrations.push(directory.join(REGISTRATION_FILE));
        }
        let output = root.path().join("ceremony");
        let epoch = "11".repeat(32);

        prepare(&genesis.path, 2, &epoch, &registrations, &output)?;

        let manifest: Manifest =
            toml::from_str(&fs_err::read_to_string(output.join(MANIFEST_FILE))?)?;
        let decryption_bytes = fs_err::read(output.join(DECRYPTION_CONFIG_FILE))?;
        let context_bytes = fs_err::read(output.join(CONTEXT_CONFIG_FILE))?;
        let decryption: DkgConfig<StorageGroup> = from_wire_bytes(&decryption_bytes)?;
        let context: DkgConfig<StorageGroup> = from_wire_bytes(&context_bytes)?;

        assert_eq!(manifest.threshold, 2);
        assert_eq!(manifest.epoch, epoch);
        assert_eq!(manifest.decryption_config_sha256, sha256_hex(&decryption_bytes));
        assert_eq!(manifest.context_config_sha256, sha256_hex(&context_bytes));
        assert_eq!(decryption.threshold, 2);
        assert_eq!(context.threshold, 2);
        assert_eq!(decryption.registry, context.registry);
        assert_eq!(context.session_id, derive_context_session_id(decryption.session_id));
        for ((position, participant), validator_key) in
            manifest.participants.iter().enumerate().zip(&genesis.validator_keys)
        {
            assert_eq!(participant.participant_index, u32::try_from(position + 1)?);
            assert_eq!(participant.validator_public_key, hex::encode(validator_key.to_bytes()));
        }
        Ok(())
    }

    #[tokio::test]
    async fn identity_rejects_signer_outside_genesis() -> TestResult {
        let root = tempfile::tempdir()?;
        let genesis = write_genesis(root.path())?;
        let outsider = ValidatorSigner::new_local(SigningKey::new());

        let error = generate_identity(&genesis.path, &outsider, &root.path().join("identity"))
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("not committed by genesis"));
        Ok(())
    }

    #[tokio::test]
    async fn prepare_rejects_substituted_dkg_identity() -> TestResult {
        let root = tempfile::tempdir()?;
        let genesis = write_genesis(root.path())?;
        let mut registrations = Vec::new();
        for (position, signing_key) in genesis.signing_keys.iter().enumerate() {
            let directory = root.path().join(format!("identity-{position}"));
            generate_identity(
                &genesis.path,
                &ValidatorSigner::new_local(signing_key.clone()),
                &directory,
            )
            .await?;
            registrations.push(directory.join(REGISTRATION_FILE));
        }

        let mut registration = read_registration(&registrations[0])?;
        let replacement_secret = StorageScalar::random(&mut OsRng);
        registration.dkg_identity_public_key = hex::encode(StorageGroup::encode_element(
            &StorageGroup::mul_generator(&replacement_secret),
        ));
        fs_err::write(&registrations[0], toml::to_string_pretty(&registration)?)?;

        let error = prepare(
            &genesis.path,
            2,
            &"22".repeat(32),
            &registrations,
            &root.path().join("ceremony"),
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("invalid validator signature"),
            "unexpected error: {error:#}",
        );
        Ok(())
    }
}
