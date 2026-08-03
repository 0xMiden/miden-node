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
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::PublicKey;
use miden_protocol::utils::serde::{Deserializable, Serializable};
use rand_core_06::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

type StorageGroup = Secp256k1GoldenGroup;
type StorageScalar = <StorageGroup as GoldenGroup>::Scalar;

const REGISTRATION_VERSION: &str = "miden-golden-dkg-registration-v1";
const MANIFEST_VERSION: &str = "miden-golden-dkg-manifest-v1";
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
        /// Hex-encoded validator signing public key committed by genesis.
        #[arg(long, value_name = "HEX")]
        validator_public_key: String,

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
    validator_public_key: String,
    dkg_identity_public_key: String,
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
pub fn run(options: GoldenDkgOptions) -> anyhow::Result<()> {
    match options.command {
        GoldenDkgCommand::Identity { validator_public_key, output_directory } => {
            generate_identity(&validator_public_key, &output_directory)
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
fn generate_identity(validator_public_key: &str, output_directory: &Path) -> anyhow::Result<()> {
    let validator_public_key = decode_validator_public_key(validator_public_key)?;
    let identity_secret = StorageScalar::random(&mut OsRng);
    ensure!(!bool::from(identity_secret.is_zero()), "generated a zero DKG identity secret");
    let identity_public_key = StorageGroup::mul_generator(&identity_secret);

    let registration = Registration {
        version: REGISTRATION_VERSION.to_owned(),
        validator_public_key: hex::encode(validator_public_key.to_bytes()),
        dkg_identity_public_key: hex::encode(StorageGroup::encode_element(&identity_public_key)),
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
    let genesis = GenesisBlock::try_from(read_genesis_block(genesis_path)?)
        .context("failed to validate genesis block")?;
    let validator_keys = genesis.inner().header().validator_keys().as_keys();

    ensure!(
        registration_paths.len() == validator_keys.len(),
        "expected {} registrations, got {}",
        validator_keys.len(),
        registration_paths.len(),
    );

    let mut registrations = BTreeMap::new();
    let mut identity_keys = BTreeSet::new();
    for path in registration_paths {
        let registration = read_registration(path)?;
        let validator_key = decode_validator_public_key(&registration.validator_public_key)?;
        let validator_key_bytes = validator_key.to_bytes();
        let identity_key = decode_identity_public_key(&registration.dkg_identity_public_key)?;
        let identity_key_bytes = StorageGroup::encode_element(&identity_key).as_ref().to_vec();

        ensure!(
            identity_keys.insert(identity_key_bytes),
            "duplicate DKG identity public key in {}",
            path.display(),
        );
        ensure!(
            registrations
                .insert(validator_key_bytes, (registration, identity_key))
                .is_none(),
            "duplicate validator registration in {}",
            path.display(),
        );
    }

    let mut registry_entries = Vec::with_capacity(validator_keys.len());
    let mut participants = Vec::with_capacity(validator_keys.len());
    for (offset, validator_key) in validator_keys.iter().enumerate() {
        let validator_key_hex = hex::encode(validator_key.to_bytes());
        let (_, identity_key) =
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
        genesis_commitment: hex::encode(genesis.inner().header().commitment().to_bytes()),
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

/// Parses a validator public key and requires its canonical hex form.
fn decode_validator_public_key(value: &str) -> anyhow::Result<PublicKey> {
    let bytes = decode_hex(value, "validator public key")?;
    let public_key = PublicKey::read_from_bytes(&bytes).context("invalid validator public key")?;
    ensure!(public_key.to_bytes() == bytes, "non-canonical validator public key");
    Ok(public_key)
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
    let temporary = temporary.keep();
    fs_err::rename(&temporary, output_directory).context("failed to publish output directory")?;
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

    /// Creates a genesis block for three validators and returns their canonical public keys.
    fn write_genesis(root: &Path) -> TestResultWith<Vec<PublicKey>> {
        let signing_keys = [SigningKey::new(), SigningKey::new(), SigningKey::new()];
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
        Ok(genesis.inner().header().validator_keys().as_keys().to_vec())
    }

    type TestResultWith<T> = Result<T, Box<dyn std::error::Error>>;

    #[test]
    fn identity_round_trip_matches_public_registration() -> TestResult {
        let root = tempfile::tempdir()?;
        let validator_key = SigningKey::new().public_key();
        let output = root.path().join("identity");

        generate_identity(&hex::encode(validator_key.to_bytes()), &output)?;

        let registration = read_registration(&output.join(REGISTRATION_FILE))?;
        let secret_bytes = Zeroizing::new(fs_err::read(output.join(IDENTITY_SECRET_FILE))?);
        let secret = decode_identity_secret(&secret_bytes)?;
        let public_key = decode_identity_public_key(&registration.dkg_identity_public_key)?;
        assert_eq!(StorageGroup::mul_generator(&secret), public_key);
        assert_eq!(registration.validator_public_key, hex::encode(validator_key.to_bytes()));

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

    #[test]
    fn prepare_binds_configs_to_canonical_genesis_order() -> TestResult {
        let root = tempfile::tempdir()?;
        let validator_keys = write_genesis(root.path())?;
        let mut registrations = Vec::new();
        for (position, validator_key) in validator_keys.iter().rev().enumerate() {
            let directory = root.path().join(format!("identity-{position}"));
            generate_identity(&hex::encode(validator_key.to_bytes()), &directory)?;
            registrations.push(directory.join(REGISTRATION_FILE));
        }
        let output = root.path().join("ceremony");
        let genesis_path = root.path().join("genesis/genesis.dat");
        let epoch = "11".repeat(32);

        prepare(&genesis_path, 2, &epoch, &registrations, &output)?;

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
            manifest.participants.iter().enumerate().zip(&validator_keys)
        {
            assert_eq!(participant.participant_index, u32::try_from(position + 1)?);
            assert_eq!(participant.validator_public_key, hex::encode(validator_key.to_bytes()));
        }
        Ok(())
    }

    #[test]
    fn prepare_rejects_registration_outside_genesis() -> TestResult {
        let root = tempfile::tempdir()?;
        let validator_keys = write_genesis(root.path())?;
        let mut registrations = Vec::new();
        for (position, validator_key) in validator_keys.iter().take(2).enumerate() {
            let directory = root.path().join(format!("identity-{position}"));
            generate_identity(&hex::encode(validator_key.to_bytes()), &directory)?;
            registrations.push(directory.join(REGISTRATION_FILE));
        }
        let outsider = root.path().join("identity-outsider");
        generate_identity(&hex::encode(SigningKey::new().public_key().to_bytes()), &outsider)?;
        registrations.push(outsider.join(REGISTRATION_FILE));

        assert!(
            prepare(
                &root.path().join("genesis/genesis.dat"),
                2,
                &"22".repeat(32),
                &registrations,
                &root.path().join("ceremony"),
            )
            .is_err(),
        );
        Ok(())
    }
}
