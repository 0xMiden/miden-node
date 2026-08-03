use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, ensure};
use golden_core::wire::{WireMessage, from_wire_bytes as from_core_wire_bytes, to_wire_bytes};
use golden_core::{
    DealerMessage,
    DkgConfig,
    DkgDealing,
    EvrfProofBackend,
    GoldenGroup,
    GoldenScalar,
    ParticipantIndex,
    ParticipantRegistry,
    SessionId,
    Share,
    TranscriptBuilder,
    complete,
    create_dealing,
    create_dealing_with_secret,
};
use golden_ehtdh1::wire::to_wire_bytes as to_ehtdh1_wire_bytes;
use golden_ehtdh1::{
    Ehtdh1Material,
    SetupContext,
    derive_context_session_id,
    material_from_dkg_outputs,
};
use golden_evrf::paper::secp_secq::SecpSecqBackend;
use golden_halo2curves::golden_group::Secp256k1GoldenGroup;
use miden_node_store::genesis::GenesisBlock;
use miden_node_utils::genesis::read_genesis_block;
use miden_protocol::Word;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{PublicKey, Signature};
use miden_protocol::crypto::hash::rpo::Rpo256;
use miden_protocol::utils::serde::{Deserializable, Serializable};
use miden_validator::{EncodedGoldenOperatorKey, StorageKeyEpoch, ValidatorSigner};
use rand_core_06::{CryptoRngCore, OsRng};
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
const DECRYPTION_DEALING_FILE: &str = "decryption-dealing.wire";
const CONTEXT_DEALING_FILE: &str = "context-dealing.wire";
const PRIVATE_STATE_FILE: &str = "private-state.wire";
const PRIVATE_STATE_MAGIC: &[u8] = b"miden-golden-dkg-local-state-v1\0";
const EPOCH_FILE: &str = "epoch.hex";
const SETUP_CONTEXT_FILE: &str = "setup-context.wire";
const PUBLIC_KEY_SET_FILE: &str = "public-key-set.wire";
const SECRET_SHARE_FILE: &str = "secret-share.wire";
const TRANSCRIPT_VERSION: &str = "miden-golden-dkg-transcript-v1";
const TRANSCRIPT_ACCEPTANCE_VERSION: &str = "miden-golden-dkg-transcript-acceptance-v1";
const TRANSCRIPT_SIGNATURE_DOMAIN: &[u8] = b"miden-golden-dkg-transcript-signature-v1";
const TRANSCRIPT_FILE: &str = "transcript.toml";
const TRANSCRIPT_ACCEPTANCE_FILE: &str = "transcript-acceptance.toml";
const TRANSCRIPT_ACCEPTANCES_FILE: &str = "transcript-acceptances.toml";

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

    /// Creates this validator's public dealings and private local state.
    Deal {
        /// Trusted genesis block for the network.
        #[arg(long, value_name = "FILE")]
        genesis: PathBuf,

        /// Directory containing the shared ceremony manifest and configurations.
        #[arg(long, value_name = "DIR")]
        ceremony_directory: PathBuf,

        /// This validator's private DKG identity file.
        #[arg(long, value_name = "FILE")]
        identity_secret: PathBuf,

        /// New directory that receives public dealings and private local state.
        #[arg(long, value_name = "DIR")]
        output_directory: PathBuf,
    },

    /// Signs the common manifest and dealing transcript.
    Accept {
        /// Trusted genesis block for the network.
        #[arg(long, value_name = "FILE")]
        genesis: PathBuf,

        /// Directory containing the shared ceremony manifest and configurations.
        #[arg(long, value_name = "DIR")]
        ceremony_directory: PathBuf,

        /// Validator signing key committed by genesis.
        #[command(flatten)]
        signing_key: ValidatorSigningKey,

        /// Public decryption-round dealing. Repeat once per genesis validator.
        #[arg(long, required = true, value_name = "FILE")]
        decryption_dealing: Vec<PathBuf>,

        /// Public context-round dealing. Repeat once per genesis validator.
        #[arg(long, required = true, value_name = "FILE")]
        context_dealing: Vec<PathBuf>,

        /// New directory that receives the transcript and this validator's acceptance.
        #[arg(long, value_name = "DIR")]
        output_directory: PathBuf,
    },

    /// Completes both DKG rounds and writes this validator's startup bundle.
    Finalize {
        /// Trusted genesis block for the network.
        #[arg(long, value_name = "FILE")]
        genesis: PathBuf,

        /// Directory containing the shared ceremony manifest and configurations.
        #[arg(long, value_name = "DIR")]
        ceremony_directory: PathBuf,

        /// This validator's private DKG identity file.
        #[arg(long, value_name = "FILE")]
        identity_secret: PathBuf,

        /// Private state produced by this validator's `deal` command.
        #[arg(long, value_name = "FILE")]
        private_state: PathBuf,

        /// Public decryption-round dealing. Repeat once per genesis validator.
        #[arg(long, required = true, value_name = "FILE")]
        decryption_dealing: Vec<PathBuf>,

        /// Public context-round dealing. Repeat once per genesis validator.
        #[arg(long, required = true, value_name = "FILE")]
        context_dealing: Vec<PathBuf>,

        /// Canonical transcript accepted by every genesis validator.
        #[arg(long, value_name = "FILE")]
        transcript: PathBuf,

        /// Signed transcript acceptance. Repeat once per genesis validator.
        #[arg(long, required = true, value_name = "FILE")]
        transcript_acceptance: Vec<PathBuf>,

        /// New directory that receives this validator's startup bundle.
        #[arg(long, value_name = "DIR")]
        output_directory: PathBuf,
    },

    /// Checks one startup bundle against genesis and the ceremony manifest.
    Validate {
        /// Trusted genesis block for the network.
        #[arg(long, value_name = "FILE")]
        genesis: PathBuf,

        /// Directory containing the shared ceremony manifest and configurations.
        #[arg(long, value_name = "DIR")]
        ceremony_directory: PathBuf,

        /// Genesis validator public key that owns this bundle.
        #[arg(long, value_name = "HEX")]
        validator_public_key: String,

        /// Directory containing the final storage-key bundle.
        #[arg(long, value_name = "DIR")]
        bundle_directory: PathBuf,
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

struct Ceremony {
    manifest: Manifest,
    manifest_sha256: [u8; 32],
    genesis_commitment: Word,
    decryption_config: DkgConfig<StorageGroup>,
    context_config: DkgConfig<StorageGroup>,
}

struct PrivateState {
    participant: ParticipantIndex,
    decryption_session_id: SessionId,
    context_session_id: SessionId,
    decryption_message_sha256: [u8; 32],
    context_message_sha256: [u8; 32],
    decryption_private_share: StorageScalar,
    context_private_share: StorageScalar,
}

#[derive(Debug, Deserialize, Serialize)]
struct CeremonyTranscript {
    version: String,
    manifest_sha256: String,
    decryption_transcript_root: String,
    context_transcript_root: String,
    decryption_dealings: Vec<TranscriptDealing>,
    context_dealings: Vec<TranscriptDealing>,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
struct TranscriptDealing {
    participant_index: u32,
    sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct TranscriptAcceptance {
    version: String,
    validator_public_key: String,
    transcript_sha256: String,
    validator_signature: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct TranscriptAcceptances {
    acceptances: Vec<TranscriptAcceptance>,
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
        GoldenDkgCommand::Deal {
            genesis,
            ceremony_directory,
            identity_secret,
            output_directory,
        } => deal::<SecpSecqBackend>(
            &genesis,
            &ceremony_directory,
            &identity_secret,
            &output_directory,
            &mut OsRng,
        ),
        GoldenDkgCommand::Accept {
            genesis,
            ceremony_directory,
            signing_key,
            decryption_dealing,
            context_dealing,
            output_directory,
        } => {
            let signer = signing_key.into_signer().await?;
            accept_transcript::<SecpSecqBackend>(
                &genesis,
                &ceremony_directory,
                &signer,
                &decryption_dealing,
                &context_dealing,
                &output_directory,
            )
            .await
        },
        GoldenDkgCommand::Finalize {
            genesis,
            ceremony_directory,
            identity_secret,
            private_state,
            decryption_dealing,
            context_dealing,
            transcript,
            transcript_acceptance,
            output_directory,
        } => finalize::<SecpSecqBackend>(
            &genesis,
            &ceremony_directory,
            &identity_secret,
            &private_state,
            &decryption_dealing,
            &context_dealing,
            &transcript,
            &transcript_acceptance,
            &output_directory,
        ),
        GoldenDkgCommand::Validate {
            genesis,
            ceremony_directory,
            validator_public_key,
            bundle_directory,
        } => {
            validate_bundle(&genesis, &ceremony_directory, &validator_public_key, &bundle_directory)
        },
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

/// Creates this validator's two public dealings and private self shares.
fn deal<B>(
    genesis_path: &Path,
    ceremony_directory: &Path,
    identity_secret_path: &Path,
    output_directory: &Path,
    rng: &mut impl CryptoRngCore,
) -> anyhow::Result<()>
where
    B: EvrfProofBackend<StorageGroup>,
    B::Proof: WireMessage,
{
    let ceremony = read_ceremony(genesis_path, ceremony_directory)?;
    let identity_secret_bytes =
        Zeroizing::new(fs_err::read(identity_secret_path).with_context(|| {
            format!("failed to read DKG identity secret {}", identity_secret_path.display())
        })?);
    let identity_secret = decode_identity_secret(&identity_secret_bytes)?;
    let participant = participant_for_identity(&ceremony.manifest, &identity_secret)?;

    println!("Creating Golden decryption dealing for participant {}.", participant.get());
    let started = Instant::now();
    let decryption = create_dealing::<StorageGroup, B>(
        participant,
        &identity_secret,
        &ceremony.decryption_config,
        rng,
    )
    .context("failed to create decryption dealing")?;
    println!(
        "Created Golden decryption dealing for participant {} in {:.1?}.",
        participant.get(),
        started.elapsed(),
    );

    println!("Creating Golden context dealing for participant {}.", participant.get());
    let started = Instant::now();
    let context = create_dealing_with_secret::<StorageGroup, B>(
        participant,
        &identity_secret,
        StorageScalar::zero(),
        &ceremony.context_config,
        rng,
    )
    .context("failed to create context dealing")?;
    println!(
        "Created Golden context dealing for participant {} in {:.1?}.",
        participant.get(),
        started.elapsed(),
    );

    let decryption_message = to_wire_bytes(&decryption.message);
    let context_message = to_wire_bytes(&context.message);
    let state = PrivateState {
        participant,
        decryption_session_id: ceremony.decryption_config.session_id,
        context_session_id: ceremony.context_config.session_id,
        decryption_message_sha256: sha256(&decryption_message),
        context_message_sha256: sha256(&context_message),
        decryption_private_share: decryption.private_share.value,
        context_private_share: context.private_share.value,
    };
    let state = encode_private_state(&state);

    publish_directory(output_directory, |directory| {
        write_new_file(&directory.join(DECRYPTION_DEALING_FILE), &decryption_message, false)?;
        write_new_file(&directory.join(CONTEXT_DEALING_FILE), &context_message, false)?;
        write_new_file(&directory.join(PRIVATE_STATE_FILE), &state, true)
    })?;

    println!("Golden DKG dealings written to {}.", output_directory.display());
    Ok(())
}

/// Signs the exact manifest and public dealings accepted by one validator.
async fn accept_transcript<B>(
    genesis_path: &Path,
    ceremony_directory: &Path,
    signer: &ValidatorSigner,
    decryption_dealing_paths: &[PathBuf],
    context_dealing_paths: &[PathBuf],
    output_directory: &Path,
) -> anyhow::Result<()>
where
    B: EvrfProofBackend<StorageGroup>,
    B::Proof: WireMessage,
{
    let ceremony = read_ceremony(genesis_path, ceremony_directory)?;
    let validator_public_key = signer.public_key();
    ensure!(
        ceremony
            .manifest
            .participants
            .iter()
            .any(|participant| participant.validator_public_key
                == hex::encode(validator_public_key.to_bytes())),
        "validator signing key is not part of this ceremony",
    );
    let (transcript, transcript_bytes) =
        build_transcript::<B>(&ceremony, decryption_dealing_paths, context_dealing_paths)?;
    let transcript_sha256 = sha256(&transcript_bytes);
    let signature = signer
        .sign_commitment(transcript_signature_commitment(
            ceremony.genesis_commitment,
            transcript_sha256,
        ))
        .await
        .context("failed to sign DKG transcript")?;
    let acceptance = TranscriptAcceptance {
        version: TRANSCRIPT_ACCEPTANCE_VERSION.to_owned(),
        validator_public_key: hex::encode(validator_public_key.to_bytes()),
        transcript_sha256: hex::encode(transcript_sha256),
        validator_signature: hex::encode(signature.to_bytes()),
    };
    let acceptance =
        toml::to_string_pretty(&acceptance).context("failed to encode transcript acceptance")?;
    debug_assert_eq!(transcript.manifest_sha256, hex::encode(ceremony.manifest_sha256));

    publish_directory(output_directory, |directory| {
        write_new_file(&directory.join(TRANSCRIPT_FILE), &transcript_bytes, false)?;
        write_new_file(&directory.join(TRANSCRIPT_ACCEPTANCE_FILE), acceptance.as_bytes(), false)
    })?;
    println!("Golden DKG transcript accepted in {}.", output_directory.display());
    Ok(())
}

/// Completes both DKG rounds and publishes one validated operator bundle.
#[expect(
    clippy::too_many_arguments,
    reason = "the ceremony files stay explicit at the CLI boundary"
)]
fn finalize<B>(
    genesis_path: &Path,
    ceremony_directory: &Path,
    identity_secret_path: &Path,
    private_state_path: &Path,
    decryption_dealing_paths: &[PathBuf],
    context_dealing_paths: &[PathBuf],
    transcript_path: &Path,
    transcript_acceptance_paths: &[PathBuf],
    output_directory: &Path,
) -> anyhow::Result<()>
where
    B: EvrfProofBackend<StorageGroup>,
    B::Proof: WireMessage,
{
    let ceremony = read_ceremony(genesis_path, ceremony_directory)?;
    let identity_secret_bytes =
        Zeroizing::new(fs_err::read(identity_secret_path).with_context(|| {
            format!("failed to read DKG identity secret {}", identity_secret_path.display())
        })?);
    let identity_secret = decode_identity_secret(&identity_secret_bytes)?;
    let participant = participant_for_identity(&ceremony.manifest, &identity_secret)?;
    let private_state_bytes =
        Zeroizing::new(fs_err::read(private_state_path).with_context(|| {
            format!("failed to read private DKG state {}", private_state_path.display())
        })?);
    let private_state = decode_private_state(&private_state_bytes)?;
    validate_private_state(&private_state, participant, &ceremony)?;

    let (transcript, transcript_bytes) = read_transcript(transcript_path, &ceremony)?;
    let acceptances = read_transcript_acceptances(
        transcript_acceptance_paths,
        &ceremony,
        sha256(&transcript_bytes),
    )?;

    let decryption_dealings =
        read_dealings::<B>(decryption_dealing_paths, ceremony.manifest.participants.len())?;
    let context_dealings =
        read_dealings::<B>(context_dealing_paths, ceremony.manifest.participants.len())?;
    validate_dealings_against_transcript::<B>(
        &decryption_dealings,
        decryption_dealing_paths,
        &transcript.decryption_dealings,
        &transcript.decryption_transcript_root,
    )?;
    validate_dealings_against_transcript::<B>(
        &context_dealings,
        context_dealing_paths,
        &transcript.context_dealings,
        &transcript.context_transcript_root,
    )?;

    println!(
        "Completing Golden decryption round for participant {} with {} dealings.",
        participant.get(),
        decryption_dealings.len(),
    );
    let started = Instant::now();
    let decryption_output = complete_round::<B>(
        participant,
        &identity_secret,
        &private_state.decryption_private_share,
        private_state.decryption_message_sha256,
        decryption_dealings,
        &ceremony.decryption_config,
    )
    .context("failed to complete decryption round")?;
    println!(
        "Completed Golden decryption round for participant {} in {:.1?}.",
        participant.get(),
        started.elapsed(),
    );

    println!(
        "Completing Golden context round for participant {} with {} dealings.",
        participant.get(),
        context_dealings.len(),
    );
    let started = Instant::now();
    let context_output = complete_round::<B>(
        participant,
        &identity_secret,
        &private_state.context_private_share,
        private_state.context_message_sha256,
        context_dealings,
        &ceremony.context_config,
    )
    .context("failed to complete context round")?;
    println!(
        "Completed Golden context round for participant {} in {:.1?}.",
        participant.get(),
        started.elapsed(),
    );

    let epoch = decode_fixed_hex::<32>(&ceremony.manifest.epoch, "storage-key epoch")?;
    let material = material_from_dkg_outputs(
        &ceremony.decryption_config,
        &decryption_output,
        &ceremony.context_config,
        &context_output,
        epoch,
    )
    .context("failed to bridge DKG outputs to EHTDH1")?;
    publish_operator_bundle(
        &material,
        &ceremony,
        &transcript_bytes,
        &acceptances,
        output_directory,
    )?;
    println!("Golden storage key bundle written to {}.", output_directory.display());
    Ok(())
}

/// Validates and publishes one final Golden operator key bundle.
fn publish_operator_bundle(
    material: &Ehtdh1Material<StorageGroup>,
    ceremony: &Ceremony,
    transcript_bytes: &[u8],
    acceptances: &TranscriptAcceptances,
    output_directory: &Path,
) -> anyhow::Result<()> {
    let epoch = decode_fixed_hex::<32>(&ceremony.manifest.epoch, "storage-key epoch")?;
    let setup_context = to_ehtdh1_wire_bytes(&material.setup_context);
    let public_key_set = to_ehtdh1_wire_bytes(&material.public_key_set);
    let secret_share = Zeroizing::new(to_ehtdh1_wire_bytes(&material.secret_share));
    EncodedGoldenOperatorKey::new(
        StorageKeyEpoch::new(epoch),
        setup_context.clone(),
        public_key_set.clone(),
        secret_share.to_vec(),
    )
    .decode()
    .context("generated invalid Golden operator key")?;

    publish_directory(output_directory, |directory| {
        write_new_file(&directory.join(EPOCH_FILE), ceremony.manifest.epoch.as_bytes(), false)?;
        write_new_file(&directory.join(SETUP_CONTEXT_FILE), &setup_context, false)?;
        write_new_file(&directory.join(PUBLIC_KEY_SET_FILE), &public_key_set, false)?;
        write_new_file(&directory.join(SECRET_SHARE_FILE), &secret_share, true)?;
        write_new_file(&directory.join(TRANSCRIPT_FILE), transcript_bytes, false)?;
        write_new_file(
            &directory.join(TRANSCRIPT_ACCEPTANCES_FILE),
            toml::to_string_pretty(acceptances)?.as_bytes(),
            false,
        )
    })?;
    Ok(())
}

/// Validates one final operator bundle and its genesis owner binding.
fn validate_bundle(
    genesis_path: &Path,
    ceremony_directory: &Path,
    validator_public_key: &str,
    bundle_directory: &Path,
) -> anyhow::Result<()> {
    let ceremony = read_ceremony(genesis_path, ceremony_directory)?;
    let transcript_path = bundle_directory.join(TRANSCRIPT_FILE);
    let (transcript, transcript_bytes) = read_transcript(&transcript_path, &ceremony)?;
    let acceptance_text =
        fs_err::read_to_string(bundle_directory.join(TRANSCRIPT_ACCEPTANCES_FILE))
            .context("failed to read transcript acceptances")?;
    let acceptances: TranscriptAcceptances =
        toml::from_str(&acceptance_text).context("failed to decode transcript acceptances")?;
    validate_transcript_acceptances(&acceptances, &ceremony, sha256(&transcript_bytes))?;
    let validator_public_key = decode_validator_public_key(validator_public_key)?;
    let expected = ceremony
        .manifest
        .participants
        .iter()
        .find(|entry| entry.validator_public_key == hex::encode(validator_public_key.to_bytes()))
        .context("validator public key is not part of this ceremony")?;
    let expected_participant = ParticipantIndex::new(expected.participant_index)?;
    let epoch_text = fs_err::read_to_string(bundle_directory.join(EPOCH_FILE))
        .context("failed to read storage-key epoch file")?;
    ensure!(
        epoch_text == ceremony.manifest.epoch,
        "storage-key epoch does not match manifest"
    );
    let epoch = decode_fixed_hex::<32>(&epoch_text, "storage-key epoch")?;
    let operator_key = EncodedGoldenOperatorKey::new(
        StorageKeyEpoch::new(epoch),
        fs_err::read(bundle_directory.join(SETUP_CONTEXT_FILE))?,
        fs_err::read(bundle_directory.join(PUBLIC_KEY_SET_FILE))?,
        fs_err::read(bundle_directory.join(SECRET_SHARE_FILE))?,
    )
    .decode()
    .context("invalid Golden operator key bundle")?;
    ensure!(
        operator_key.participant() == expected_participant,
        "bundle belongs to participant {}, expected {}",
        operator_key.participant().get(),
        expected_participant.get(),
    );
    validate_setup_context(operator_key.setup_context(), &ceremony)?;
    ensure!(
        operator_key.setup_context().decryption_transcript_root
            == decode_fixed_hex::<32>(
                &transcript.decryption_transcript_root,
                "decryption transcript root",
            )?
            && operator_key.setup_context().context_transcript_root
                == decode_fixed_hex::<32>(
                    &transcript.context_transcript_root,
                    "context transcript root",
                )?,
        "bundle transcript roots do not match accepted transcript",
    );
    println!(
        "Golden storage key bundle is valid for participant {}.",
        expected_participant.get(),
    );
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

/// Reads a ceremony directory and checks every public value against genesis.
fn read_ceremony(genesis_path: &Path, directory: &Path) -> anyhow::Result<Ceremony> {
    let manifest_path = directory.join(MANIFEST_FILE);
    let manifest_text = fs_err::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read DKG manifest {}", manifest_path.display()))?;
    let manifest: Manifest =
        toml::from_str(&manifest_text).context("failed to decode DKG manifest")?;
    ensure!(manifest.version == MANIFEST_VERSION, "unsupported DKG manifest version");

    let genesis = read_trusted_genesis(genesis_path)?;
    let genesis_commitment = genesis.inner().header().commitment();
    ensure!(
        manifest.genesis_commitment == hex::encode(genesis_commitment.to_bytes()),
        "DKG manifest belongs to a different genesis block",
    );
    decode_fixed_hex::<32>(&manifest.epoch, "storage-key epoch")?;

    let decryption_bytes = fs_err::read(directory.join(DECRYPTION_CONFIG_FILE))
        .context("failed to read decryption configuration")?;
    let context_bytes = fs_err::read(directory.join(CONTEXT_CONFIG_FILE))
        .context("failed to read context configuration")?;
    ensure!(
        sha256_hex(&decryption_bytes) == manifest.decryption_config_sha256,
        "decryption configuration digest does not match manifest",
    );
    ensure!(
        sha256_hex(&context_bytes) == manifest.context_config_sha256,
        "context configuration digest does not match manifest",
    );
    let decryption_config = from_core_wire_bytes::<DkgConfig<StorageGroup>>(&decryption_bytes)
        .context("invalid decryption configuration")?;
    let context_config = from_core_wire_bytes::<DkgConfig<StorageGroup>>(&context_bytes)
        .context("invalid context configuration")?;

    ensure!(decryption_config.threshold == manifest.threshold, "threshold mismatch");
    ensure!(context_config.threshold == manifest.threshold, "context threshold mismatch");
    ensure!(
        hex::encode(decryption_config.beta.to_repr()) == manifest.beta
            && context_config.beta == decryption_config.beta,
        "DKG beta mismatch",
    );
    ensure!(
        hex::encode(decryption_config.session_id.0) == manifest.decryption_session_id,
        "decryption session mismatch",
    );
    ensure!(
        hex::encode(context_config.session_id.0) == manifest.context_session_id
            && context_config.session_id == derive_context_session_id(decryption_config.session_id),
        "context session mismatch",
    );
    ensure!(
        context_config.registry.root() == decryption_config.registry.root(),
        "registry mismatch between DKG rounds",
    );

    let validator_keys = genesis.inner().header().validator_keys().as_keys();
    ensure!(
        manifest.participants.len() == validator_keys.len(),
        "manifest participant count does not match genesis",
    );
    for (offset, (entry, validator_key)) in
        manifest.participants.iter().zip(validator_keys).enumerate()
    {
        let participant = ParticipantIndex::new(
            u32::try_from(offset + 1).context("too many Golden DKG participants")?,
        )?;
        ensure!(entry.participant_index == participant.get(), "non-canonical participant order");
        ensure!(
            entry.validator_public_key == hex::encode(validator_key.to_bytes()),
            "manifest validator order does not match genesis",
        );
        let expected_identity = decode_identity_public_key(&entry.dkg_identity_public_key)?;
        ensure!(
            decryption_config.registry.public_key(participant)? == &expected_identity
                && context_config.registry.public_key(participant)? == &expected_identity,
            "manifest identity does not match DKG registry",
        );
    }

    Ok(Ceremony {
        manifest,
        manifest_sha256: sha256(manifest_text.as_bytes()),
        genesis_commitment,
        decryption_config,
        context_config,
    })
}

/// Returns the manifest participant whose public identity matches a secret.
fn participant_for_identity(
    manifest: &Manifest,
    identity_secret: &StorageScalar,
) -> anyhow::Result<ParticipantIndex> {
    let public_key = StorageGroup::mul_generator(identity_secret);
    let public_key = hex::encode(StorageGroup::encode_element(&public_key));
    let entry = manifest
        .participants
        .iter()
        .find(|entry| entry.dkg_identity_public_key == public_key)
        .context("DKG identity is not part of this ceremony")?;
    Ok(ParticipantIndex::new(entry.participant_index)?)
}

/// Reads exactly one public dealing from every ceremony participant.
fn read_dealings<B>(
    paths: &[PathBuf],
    expected: usize,
) -> anyhow::Result<BTreeMap<ParticipantIndex, DealerMessage<StorageGroup, B::Proof>>>
where
    B: EvrfProofBackend<StorageGroup>,
    B::Proof: WireMessage,
{
    ensure!(paths.len() == expected, "expected {expected} dealings, got {}", paths.len());
    let mut dealings = BTreeMap::new();
    for path in paths {
        let bytes = fs_err::read(path)
            .with_context(|| format!("failed to read dealing {}", path.display()))?;
        let message = from_core_wire_bytes::<DealerMessage<StorageGroup, B::Proof>>(&bytes)
            .with_context(|| format!("invalid dealing {}", path.display()))?;
        let dealer = message.dealer;
        ensure!(
            dealings.insert(dealer, message).is_none(),
            "duplicate dealing from participant {}",
            dealer.get(),
        );
    }
    ensure!(dealings.len() == expected, "dealing set is incomplete");
    Ok(dealings)
}

/// Builds the canonical transcript over one manifest and both dealing rounds.
fn build_transcript<B>(
    ceremony: &Ceremony,
    decryption_paths: &[PathBuf],
    context_paths: &[PathBuf],
) -> anyhow::Result<(CeremonyTranscript, Vec<u8>)>
where
    B: EvrfProofBackend<StorageGroup>,
    B::Proof: WireMessage,
{
    let expected = ceremony.manifest.participants.len();
    let decryption_dealings = read_dealings::<B>(decryption_paths, expected)?;
    let context_dealings = read_dealings::<B>(context_paths, expected)?;
    let transcript = CeremonyTranscript {
        version: TRANSCRIPT_VERSION.to_owned(),
        manifest_sha256: hex::encode(ceremony.manifest_sha256),
        decryption_transcript_root: hex::encode(completion_root(&decryption_dealings)),
        context_transcript_root: hex::encode(completion_root(&context_dealings)),
        decryption_dealings: dealing_hashes::<B>(decryption_paths, expected)?,
        context_dealings: dealing_hashes::<B>(context_paths, expected)?,
    };
    let bytes = toml::to_string_pretty(&transcript)
        .context("failed to encode DKG transcript")?
        .into_bytes();
    Ok((transcript, bytes))
}

/// Reads one canonical transcript and checks its manifest binding.
fn read_transcript(
    path: &Path,
    ceremony: &Ceremony,
) -> anyhow::Result<(CeremonyTranscript, Vec<u8>)> {
    let bytes = fs_err::read(path)
        .with_context(|| format!("failed to read DKG transcript {}", path.display()))?;
    let text = std::str::from_utf8(&bytes).context("DKG transcript is not UTF-8")?;
    let transcript: CeremonyTranscript =
        toml::from_str(text).context("failed to decode DKG transcript")?;
    ensure!(transcript.version == TRANSCRIPT_VERSION, "unsupported DKG transcript version");
    ensure!(
        transcript.manifest_sha256 == hex::encode(ceremony.manifest_sha256),
        "DKG transcript belongs to another manifest",
    );
    decode_fixed_hex::<32>(&transcript.decryption_transcript_root, "decryption transcript root")?;
    decode_fixed_hex::<32>(&transcript.context_transcript_root, "context transcript root")?;
    let canonical =
        toml::to_string_pretty(&transcript).context("failed to encode DKG transcript")?;
    ensure!(canonical.as_bytes() == bytes, "non-canonical DKG transcript");
    Ok((transcript, bytes))
}

/// Reads, sorts, and verifies every validator's transcript acceptance.
fn read_transcript_acceptances(
    paths: &[PathBuf],
    ceremony: &Ceremony,
    transcript_sha256: [u8; 32],
) -> anyhow::Result<TranscriptAcceptances> {
    let mut acceptances = Vec::with_capacity(paths.len());
    for path in paths {
        let text = fs_err::read_to_string(path)
            .with_context(|| format!("failed to read transcript acceptance {}", path.display()))?;
        acceptances.push(toml::from_str(&text).with_context(|| {
            format!("failed to decode transcript acceptance {}", path.display())
        })?);
    }
    let acceptances = TranscriptAcceptances { acceptances };
    validate_transcript_acceptances(&acceptances, ceremony, transcript_sha256)?;

    let by_key = acceptances
        .acceptances
        .into_iter()
        .map(|acceptance| (acceptance.validator_public_key.clone(), acceptance))
        .collect::<BTreeMap<_, _>>();
    let mut ordered = Vec::with_capacity(by_key.len());
    for participant in &ceremony.manifest.participants {
        ordered.push(
            by_key
                .get(&participant.validator_public_key)
                .context("missing transcript acceptance")?
                .to_owned(),
        );
    }
    Ok(TranscriptAcceptances { acceptances: ordered })
}

/// Verifies unanimous genesis-validator acceptance of one exact transcript.
fn validate_transcript_acceptances(
    acceptances: &TranscriptAcceptances,
    ceremony: &Ceremony,
    transcript_sha256: [u8; 32],
) -> anyhow::Result<()> {
    ensure!(
        acceptances.acceptances.len() == ceremony.manifest.participants.len(),
        "expected {} transcript acceptances, got {}",
        ceremony.manifest.participants.len(),
        acceptances.acceptances.len(),
    );
    let expected_digest = hex::encode(transcript_sha256);
    let commitment =
        transcript_signature_commitment(ceremony.genesis_commitment, transcript_sha256);
    let mut accepted = BTreeSet::new();
    for acceptance in &acceptances.acceptances {
        ensure!(
            acceptance.version == TRANSCRIPT_ACCEPTANCE_VERSION,
            "unsupported transcript acceptance version",
        );
        ensure!(
            acceptance.transcript_sha256 == expected_digest,
            "transcript acceptance belongs to another transcript",
        );
        let validator_key = decode_validator_public_key(&acceptance.validator_public_key)?;
        let signature = decode_validator_signature(&acceptance.validator_signature)?;
        ensure!(
            signature.verify(commitment, &validator_key),
            "invalid transcript acceptance signature",
        );
        ensure!(
            accepted.insert(acceptance.validator_public_key.clone()),
            "duplicate transcript acceptance",
        );
    }
    let expected = ceremony
        .manifest
        .participants
        .iter()
        .map(|participant| participant.validator_public_key.clone())
        .collect::<BTreeSet<_>>();
    ensure!(accepted == expected, "transcript acceptances do not match genesis validators");
    Ok(())
}

/// Recomputes one round's canonical dealing hashes and completion root.
fn validate_dealings_against_transcript<B>(
    dealings: &BTreeMap<ParticipantIndex, DealerMessage<StorageGroup, B::Proof>>,
    paths: &[PathBuf],
    expected_hashes: &[TranscriptDealing],
    expected_root: &str,
) -> anyhow::Result<()>
where
    B: EvrfProofBackend<StorageGroup>,
    B::Proof: WireMessage,
{
    ensure!(
        dealing_hashes::<B>(paths, dealings.len())? == expected_hashes,
        "dealings do not match accepted transcript",
    );
    ensure!(
        hex::encode(completion_root(dealings)) == expected_root,
        "dealing roots do not match accepted transcript",
    );
    Ok(())
}

/// Returns canonical hashes for dealing files sorted by participant.
fn dealing_hashes<B>(paths: &[PathBuf], expected: usize) -> anyhow::Result<Vec<TranscriptDealing>>
where
    B: EvrfProofBackend<StorageGroup>,
    B::Proof: WireMessage,
{
    let mut hashes = BTreeMap::new();
    for path in paths {
        let bytes = fs_err::read(path)
            .with_context(|| format!("failed to read dealing {}", path.display()))?;
        let message = from_core_wire_bytes::<DealerMessage<StorageGroup, B::Proof>>(&bytes)
            .with_context(|| format!("invalid dealing {}", path.display()))?;
        ensure!(
            hashes.insert(message.dealer, sha256_hex(&bytes)).is_none(),
            "duplicate dealing from participant {}",
            message.dealer.get(),
        );
    }
    ensure!(hashes.len() == expected, "dealing set is incomplete");
    Ok(hashes
        .into_iter()
        .map(|(participant, sha256)| TranscriptDealing {
            participant_index: participant.get(),
            sha256,
        })
        .collect())
}

/// Reproduces Golden's completion transcript root from public dealings.
fn completion_root<P>(
    dealings: &BTreeMap<ParticipantIndex, DealerMessage<StorageGroup, P>>,
) -> [u8; 32] {
    let mut transcript = TranscriptBuilder::with_prefix(b"golden-core-v1", b"completion");
    transcript.bytes(b"backend", StorageGroup::BACKEND_ID.as_bytes());
    transcript.usize(b"dealings-len", dealings.len());
    for (dealer, message) in dealings {
        transcript.participant(b"dealer", *dealer);
        transcript.bytes(b"dealing-root", &message.transcript_root);
    }
    transcript.root()
}

/// Commits a validator signature to one exact public ceremony transcript.
fn transcript_signature_commitment(genesis_commitment: Word, transcript_sha256: [u8; 32]) -> Word {
    let mut bytes = Vec::with_capacity(
        TRANSCRIPT_SIGNATURE_DOMAIN.len() + Word::SERIALIZED_SIZE + transcript_sha256.len(),
    );
    bytes.extend_from_slice(TRANSCRIPT_SIGNATURE_DOMAIN);
    bytes.extend_from_slice(&genesis_commitment.to_bytes());
    bytes.extend_from_slice(&transcript_sha256);
    Rpo256::hash(&bytes)
}

/// Completes one DKG round from public messages and the local self share.
fn complete_round<B>(
    participant: ParticipantIndex,
    identity_secret: &StorageScalar,
    private_share: &StorageScalar,
    expected_own_message_sha256: [u8; 32],
    mut dealings: BTreeMap<ParticipantIndex, DealerMessage<StorageGroup, B::Proof>>,
    config: &DkgConfig<StorageGroup>,
) -> anyhow::Result<golden_core::DkgOutput<StorageGroup>>
where
    B: EvrfProofBackend<StorageGroup>,
    B::Proof: WireMessage,
{
    let own_message = dealings.remove(&participant).context("missing local dealing")?;
    ensure!(
        sha256(&to_wire_bytes(&own_message)) == expected_own_message_sha256,
        "local dealing does not match private state",
    );
    let own_dealing = DkgDealing {
        message: own_message,
        private_share: Share { participant, value: *private_share },
    };
    Ok(complete::<StorageGroup, B>(
        participant,
        identity_secret,
        &own_dealing,
        &dealings,
        config,
    )?)
}

/// Checks a generated setup context against the public ceremony.
fn validate_setup_context(context: &SetupContext, ceremony: &Ceremony) -> anyhow::Result<()> {
    ensure!(context.threshold == ceremony.manifest.threshold, "setup threshold mismatch");
    ensure!(
        context.registry_root == ceremony.decryption_config.registry.root(),
        "setup registry mismatch",
    );
    ensure!(
        context.decryption_session_id == ceremony.decryption_config.session_id
            && context.context_session_id == ceremony.context_config.session_id,
        "setup session mismatch",
    );
    ensure!(
        context.epoch == decode_fixed_hex::<32>(&ceremony.manifest.epoch, "epoch")?,
        "setup epoch mismatch"
    );
    let participants = ceremony
        .manifest
        .participants
        .iter()
        .map(|entry| ParticipantIndex::new(entry.participant_index))
        .collect::<Result<Vec<_>, _>>()?;
    ensure!(context.participants == participants, "setup participant mismatch");
    Ok(())
}

/// Encodes private self shares with their participant, sessions, and public messages.
fn encode_private_state(state: &PrivateState) -> Zeroizing<Vec<u8>> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(
        PRIVATE_STATE_MAGIC.len() + 4 + 4 * 32 + 2 * StorageScalar::REPR_BYTES,
    ));
    bytes.extend_from_slice(PRIVATE_STATE_MAGIC);
    bytes.extend_from_slice(&state.participant.get().to_be_bytes());
    bytes.extend_from_slice(&state.decryption_session_id.0);
    bytes.extend_from_slice(&state.context_session_id.0);
    bytes.extend_from_slice(&state.decryption_message_sha256);
    bytes.extend_from_slice(&state.context_message_sha256);
    bytes.extend_from_slice(state.decryption_private_share.to_repr().as_ref());
    bytes.extend_from_slice(state.context_private_share.to_repr().as_ref());
    bytes
}

/// Decodes private self shares and rejects trailing or non-canonical data.
fn decode_private_state(bytes: &[u8]) -> anyhow::Result<PrivateState> {
    let mut bytes = bytes
        .strip_prefix(PRIVATE_STATE_MAGIC)
        .context("invalid private DKG state format")?;
    let expected = 4 + 4 * 32 + 2 * StorageScalar::REPR_BYTES;
    ensure!(bytes.len() == expected, "invalid private DKG state length");
    let participant = ParticipantIndex::new(u32::from_be_bytes(take_array(&mut bytes)?))?;
    let decryption_session_id = SessionId(take_array(&mut bytes)?);
    let context_session_id = SessionId(take_array(&mut bytes)?);
    let decryption_message_sha256 = take_array(&mut bytes)?;
    let context_message_sha256 = take_array(&mut bytes)?;
    let decryption_private_share = take_scalar(&mut bytes)?;
    let context_private_share = take_scalar(&mut bytes)?;
    ensure!(bytes.is_empty(), "trailing private DKG state bytes");
    Ok(PrivateState {
        participant,
        decryption_session_id,
        context_session_id,
        decryption_message_sha256,
        context_message_sha256,
        decryption_private_share,
        context_private_share,
    })
}

/// Checks that private state belongs to this participant and ceremony.
fn validate_private_state(
    state: &PrivateState,
    participant: ParticipantIndex,
    ceremony: &Ceremony,
) -> anyhow::Result<()> {
    ensure!(
        state.participant == participant,
        "private DKG state belongs to another participant"
    );
    ensure!(
        state.decryption_session_id == ceremony.decryption_config.session_id
            && state.context_session_id == ceremony.context_config.session_id,
        "private DKG state belongs to another ceremony",
    );
    Ok(())
}

/// Removes and returns one fixed-size prefix.
fn take_array<const N: usize>(bytes: &mut &[u8]) -> anyhow::Result<[u8; N]> {
    ensure!(bytes.len() >= N, "truncated private DKG state");
    let (head, tail) = bytes.split_at(N);
    *bytes = tail;
    Ok(head.try_into().expect("fixed-size slice"))
}

/// Removes and decodes one canonical scalar.
fn take_scalar(bytes: &mut &[u8]) -> anyhow::Result<StorageScalar> {
    let scalar = take_array::<{ StorageScalar::REPR_BYTES }>(bytes)?;
    let repr = <StorageScalar as GoldenScalar>::Repr::try_from(scalar.to_vec())
        .map_err(|_| anyhow::anyhow!("invalid private DKG scalar length"))?;
    StorageScalar::from_repr(&repr).context("invalid private DKG scalar")
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
    hex::encode(sha256(bytes))
}

/// Returns the SHA-256 digest of one artifact.
fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

#[cfg(test)]
mod tests {
    use golden_core::wire::from_wire_bytes;
    use golden_ehtdh1::wire::from_wire_bytes as from_ehtdh1_wire_bytes;
    use golden_ehtdh1::{
        Combiner,
        PublicKeySet,
        SealingKey,
        SecretShare,
        SetupContext,
        UnsealingShare,
    };
    use golden_evrf::prototype::ShareOpeningBackend;
    use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey;
    use rand_chacha_03::ChaCha20Rng;
    use rand_chacha_03::rand_core::SeedableRng;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[derive(Clone)]
    struct TestGenesis {
        path: PathBuf,
        signing_keys: Vec<SigningKey>,
        validator_keys: Vec<PublicKey>,
    }

    /// Creates a genesis block for three validators.
    fn write_genesis(root: &Path) -> TestResultWith<TestGenesis> {
        write_genesis_with_validator_count(root, 3)
    }

    /// Creates a genesis block with the requested validator count.
    fn write_genesis_with_validator_count(
        root: &Path,
        validator_count: usize,
    ) -> TestResultWith<TestGenesis> {
        let signing_keys = (0..validator_count).map(|_| SigningKey::new()).collect::<Vec<_>>();
        let validators = signing_keys
            .iter()
            .map(|key| format!("\"{}\"", hex::encode(key.public_key().to_bytes())))
            .collect::<Vec<_>>()
            .join(", ");
        let config = format!(
            concat!(
                "version = 1\n",
                "timestamp = 1717344256\n",
                "validators = [{validators}]\n",
                "\n[fee_parameters]\n",
                "verification_base_fee = 0\n",
            ),
            validators = validators,
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

    #[derive(Clone)]
    struct TestCeremony {
        genesis: TestGenesis,
        ceremony: PathBuf,
        identities: Vec<PathBuf>,
    }

    /// Creates signed identities and one shared ceremony directory.
    async fn prepare_test_ceremony(
        root: &Path,
        validator_count: usize,
        threshold: usize,
    ) -> TestResultWith<TestCeremony> {
        let genesis = write_genesis_with_validator_count(root, validator_count)?;
        let mut registrations = Vec::new();
        let mut identities = Vec::new();
        for (position, signing_key) in genesis.signing_keys.iter().enumerate() {
            let directory = root.join(format!("identity-{position}"));
            generate_identity(
                &genesis.path,
                &ValidatorSigner::new_local(signing_key.clone()),
                &directory,
            )
            .await?;
            registrations.push(directory.join(REGISTRATION_FILE));
            identities.push(directory);
        }
        let ceremony = root.join("ceremony");
        prepare(&genesis.path, threshold, &"33".repeat(32), &registrations, &ceremony)?;
        Ok(TestCeremony { genesis, ceremony, identities })
    }

    /// Creates both dealings for every validator with the fast proof backend.
    fn deal_for_all(root: &Path, ceremony: &TestCeremony) -> TestResultWith<Vec<PathBuf>> {
        let mut rng = ChaCha20Rng::from_seed([41; 32]);
        let mut outputs = Vec::new();
        for (position, identity) in ceremony.identities.iter().enumerate() {
            let output = root.join(format!("deal-{position}"));
            deal::<ShareOpeningBackend>(
                &ceremony.genesis.path,
                &ceremony.ceremony,
                &identity.join(IDENTITY_SECRET_FILE),
                &output,
                &mut rng,
            )?;
            outputs.push(output);
        }
        Ok(outputs)
    }

    /// Returns one named dealing file from every participant directory.
    fn dealing_paths(outputs: &[PathBuf], name: &str) -> Vec<PathBuf> {
        outputs.iter().map(|directory| directory.join(name)).collect()
    }

    struct AcceptedTranscript {
        transcript: PathBuf,
        acceptances: Vec<PathBuf>,
    }

    /// Has every genesis validator sign the same public transcript.
    async fn accept_for_all<B>(
        root: &Path,
        ceremony: &TestCeremony,
        dealings: &[PathBuf],
    ) -> TestResultWith<AcceptedTranscript>
    where
        B: EvrfProofBackend<StorageGroup>,
        B::Proof: WireMessage,
    {
        let mut outputs = Vec::new();
        for (position, signing_key) in ceremony.genesis.signing_keys.iter().enumerate() {
            let output = root.join(format!("accept-{position}"));
            accept_transcript::<B>(
                &ceremony.genesis.path,
                &ceremony.ceremony,
                &ValidatorSigner::new_local(signing_key.clone()),
                &dealing_paths(dealings, DECRYPTION_DEALING_FILE),
                &dealing_paths(dealings, CONTEXT_DEALING_FILE),
                &output,
            )
            .await?;
            outputs.push(output);
        }
        let transcript = outputs[0].join(TRANSCRIPT_FILE);
        let expected = fs_err::read(&transcript)?;
        assert!(
            outputs
                .iter()
                .all(|output| fs_err::read(output.join(TRANSCRIPT_FILE)).unwrap() == expected)
        );
        Ok(AcceptedTranscript {
            transcript,
            acceptances: outputs
                .iter()
                .map(|output| output.join(TRANSCRIPT_ACCEPTANCE_FILE))
                .collect(),
        })
    }

    /// Completes one startup bundle with the fast proof backend.
    fn finalize_test_bundle(
        root: &Path,
        ceremony: &TestCeremony,
        dealings: &[PathBuf],
        accepted: &AcceptedTranscript,
        position: usize,
    ) -> TestResultWith<PathBuf> {
        let output = root.join(format!("bundle-{position}"));
        finalize::<ShareOpeningBackend>(
            &ceremony.genesis.path,
            &ceremony.ceremony,
            &ceremony.identities[position].join(IDENTITY_SECRET_FILE),
            &dealings[position].join(PRIVATE_STATE_FILE),
            &dealing_paths(dealings, DECRYPTION_DEALING_FILE),
            &dealing_paths(dealings, CONTEXT_DEALING_FILE),
            &accepted.transcript,
            &accepted.acceptances,
            &output,
        )?;
        Ok(output)
    }

    #[tokio::test]
    async fn three_validators_complete_dkg_and_recover_with_any_two_shares() -> TestResult {
        let root = tempfile::tempdir()?;
        let ceremony = prepare_test_ceremony(root.path(), 3, 2).await?;
        let dealings = deal_for_all(root.path(), &ceremony)?;
        let accepted =
            accept_for_all::<ShareOpeningBackend>(root.path(), &ceremony, &dealings).await?;
        let mut bundles = Vec::new();
        for position in 0..3 {
            let bundle =
                finalize_test_bundle(root.path(), &ceremony, &dealings, &accepted, position)?;
            validate_bundle(
                &ceremony.genesis.path,
                &ceremony.ceremony,
                &hex::encode(ceremony.genesis.signing_keys[position].public_key().to_bytes()),
                &bundle,
            )?;
            bundles.push(bundle);
        }

        let shared_setup = fs_err::read(bundles[0].join(SETUP_CONTEXT_FILE))?;
        let shared_public_keys = fs_err::read(bundles[0].join(PUBLIC_KEY_SET_FILE))?;
        let secret_shares = bundles
            .iter()
            .map(|bundle| fs_err::read(bundle.join(SECRET_SHARE_FILE)))
            .collect::<Result<Vec<_>, _>>()?;
        assert!(bundles.iter().all(|bundle| {
            fs_err::read(bundle.join(SETUP_CONTEXT_FILE)).unwrap() == shared_setup
                && fs_err::read(bundle.join(PUBLIC_KEY_SET_FILE)).unwrap() == shared_public_keys
        }));
        assert_ne!(secret_shares[0], secret_shares[1]);
        assert_ne!(secret_shares[1], secret_shares[2]);

        let setup: SetupContext = from_ehtdh1_wire_bytes(&shared_setup)?;
        let public_keys: PublicKeySet<StorageGroup> = from_ehtdh1_wire_bytes(&shared_public_keys)?;
        let secret_shares = secret_shares
            .iter()
            .map(|bytes| from_ehtdh1_wire_bytes::<SecretShare<StorageGroup>>(bytes))
            .collect::<Result<Vec<_>, _>>()?;
        let sealing_key = SealingKey::new(public_keys.joint_public_key)?;
        let context = b"transaction-inputs/test";
        let content_key = [0x5a; 32];
        let mut rng = ChaCha20Rng::from_seed([42; 32]);
        let ciphertext =
            sealing_key.seal_bytes_with_associated_data(&mut rng, &content_key, context)?;
        let shares = secret_shares
            .iter()
            .map(|secret| {
                UnsealingShare::new(secret.clone()).decrypt_share_with_associated_data(
                    &mut rng,
                    &setup,
                    &ciphertext,
                    context,
                    context,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let combiner = Combiner::new(public_keys, setup)?;
        for pair in [[0, 1], [0, 2], [1, 2]] {
            let recovered = combiner.combine_exact_with_associated_data(
                &ciphertext,
                context,
                context,
                &[shares[pair[0]].clone(), shares[pair[1]].clone()],
            )?;
            assert_eq!(recovered, content_key);
        }
        Ok(())
    }

    #[tokio::test]
    async fn finalize_rejects_incomplete_or_duplicate_dealings() -> TestResult {
        let root = tempfile::tempdir()?;
        let ceremony = prepare_test_ceremony(root.path(), 3, 2).await?;
        let dealings = deal_for_all(root.path(), &ceremony)?;
        let accepted =
            accept_for_all::<ShareOpeningBackend>(root.path(), &ceremony, &dealings).await?;
        let decryption = dealing_paths(&dealings, DECRYPTION_DEALING_FILE);
        let context = dealing_paths(&dealings, CONTEXT_DEALING_FILE);
        let output = root.path().join("bundle");

        assert!(
            finalize::<ShareOpeningBackend>(
                &ceremony.genesis.path,
                &ceremony.ceremony,
                &ceremony.identities[0].join(IDENTITY_SECRET_FILE),
                &dealings[0].join(PRIVATE_STATE_FILE),
                &decryption[..2],
                &context,
                &accepted.transcript,
                &accepted.acceptances,
                &output,
            )
            .is_err()
        );
        assert!(!output.exists());

        let duplicate = vec![decryption[0].clone(), decryption[0].clone(), decryption[2].clone()];
        assert!(
            finalize::<ShareOpeningBackend>(
                &ceremony.genesis.path,
                &ceremony.ceremony,
                &ceremony.identities[0].join(IDENTITY_SECRET_FILE),
                &dealings[0].join(PRIVATE_STATE_FILE),
                &duplicate,
                &context,
                &accepted.transcript,
                &accepted.acceptances,
                &output,
            )
            .is_err()
        );
        assert!(!output.exists());
        Ok(())
    }

    #[tokio::test]
    async fn finalize_rejects_tampered_dealing_without_partial_output() -> TestResult {
        let root = tempfile::tempdir()?;
        let ceremony = prepare_test_ceremony(root.path(), 3, 2).await?;
        let dealings = deal_for_all(root.path(), &ceremony)?;
        let accepted =
            accept_for_all::<ShareOpeningBackend>(root.path(), &ceremony, &dealings).await?;
        let tampered = root.path().join("tampered.wire");
        let mut bytes = fs_err::read(dealings[1].join(DECRYPTION_DEALING_FILE))?;
        let offset = bytes.len() / 2;
        bytes[offset] ^= 1;
        fs_err::write(&tampered, bytes)?;
        let mut decryption = dealing_paths(&dealings, DECRYPTION_DEALING_FILE);
        decryption[1] = tampered;
        let output = root.path().join("bundle");

        assert!(
            finalize::<ShareOpeningBackend>(
                &ceremony.genesis.path,
                &ceremony.ceremony,
                &ceremony.identities[0].join(IDENTITY_SECRET_FILE),
                &dealings[0].join(PRIVATE_STATE_FILE),
                &decryption,
                &dealing_paths(&dealings, CONTEXT_DEALING_FILE),
                &accepted.transcript,
                &accepted.acceptances,
                &output,
            )
            .is_err()
        );
        assert!(!output.exists());
        Ok(())
    }

    #[tokio::test]
    async fn finalize_rejects_valid_dealer_equivocation() -> TestResult {
        let root = tempfile::tempdir()?;
        let ceremony = prepare_test_ceremony(root.path(), 3, 2).await?;
        let dealings = deal_for_all(root.path(), &ceremony)?;
        let accepted =
            accept_for_all::<ShareOpeningBackend>(root.path(), &ceremony, &dealings).await?;

        let alternate = root.path().join("alternate-deal");
        let mut rng = ChaCha20Rng::from_seed([99; 32]);
        deal::<ShareOpeningBackend>(
            &ceremony.genesis.path,
            &ceremony.ceremony,
            &ceremony.identities[1].join(IDENTITY_SECRET_FILE),
            &alternate,
            &mut rng,
        )?;
        let mut decryption = dealing_paths(&dealings, DECRYPTION_DEALING_FILE);
        decryption[1] = alternate.join(DECRYPTION_DEALING_FILE);
        let output = root.path().join("bundle");

        let error = finalize::<ShareOpeningBackend>(
            &ceremony.genesis.path,
            &ceremony.ceremony,
            &ceremony.identities[0].join(IDENTITY_SECRET_FILE),
            &dealings[0].join(PRIVATE_STATE_FILE),
            &decryption,
            &dealing_paths(&dealings, CONTEXT_DEALING_FILE),
            &accepted.transcript,
            &accepted.acceptances,
            &output,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("accepted transcript"));
        assert!(!output.exists());
        Ok(())
    }

    #[tokio::test]
    async fn finalize_requires_every_transcript_acceptance() -> TestResult {
        let root = tempfile::tempdir()?;
        let ceremony = prepare_test_ceremony(root.path(), 3, 2).await?;
        let dealings = deal_for_all(root.path(), &ceremony)?;
        let accepted =
            accept_for_all::<ShareOpeningBackend>(root.path(), &ceremony, &dealings).await?;
        let output = root.path().join("bundle");

        let error = finalize::<ShareOpeningBackend>(
            &ceremony.genesis.path,
            &ceremony.ceremony,
            &ceremony.identities[0].join(IDENTITY_SECRET_FILE),
            &dealings[0].join(PRIVATE_STATE_FILE),
            &dealing_paths(&dealings, DECRYPTION_DEALING_FILE),
            &dealing_paths(&dealings, CONTEXT_DEALING_FILE),
            &accepted.transcript,
            &accepted.acceptances[..2],
            &output,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("expected 3 transcript acceptances"));
        assert!(!output.exists());
        Ok(())
    }

    #[tokio::test]
    async fn finalize_rejects_manifest_changed_after_acceptance() -> TestResult {
        let root = tempfile::tempdir()?;
        let ceremony = prepare_test_ceremony(root.path(), 3, 2).await?;
        let dealings = deal_for_all(root.path(), &ceremony)?;
        let accepted =
            accept_for_all::<ShareOpeningBackend>(root.path(), &ceremony, &dealings).await?;
        let manifest_path = ceremony.ceremony.join(MANIFEST_FILE);
        let mut manifest: Manifest = toml::from_str(&fs_err::read_to_string(&manifest_path)?)?;
        manifest.epoch = "55".repeat(32);
        fs_err::write(&manifest_path, toml::to_string_pretty(&manifest)?)?;
        let output = root.path().join("bundle");

        let error = finalize::<ShareOpeningBackend>(
            &ceremony.genesis.path,
            &ceremony.ceremony,
            &ceremony.identities[0].join(IDENTITY_SECRET_FILE),
            &dealings[0].join(PRIVATE_STATE_FILE),
            &dealing_paths(&dealings, DECRYPTION_DEALING_FILE),
            &dealing_paths(&dealings, CONTEXT_DEALING_FILE),
            &accepted.transcript,
            &accepted.acceptances,
            &output,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("another manifest"));
        assert!(!output.exists());
        Ok(())
    }

    #[tokio::test]
    async fn private_state_cannot_cross_ceremonies() -> TestResult {
        let root = tempfile::tempdir()?;
        let first_root = root.path().join("first");
        let second_root = root.path().join("second");
        fs_err::create_dir_all(&first_root)?;
        fs_err::create_dir_all(&second_root)?;
        let first = prepare_test_ceremony(&first_root, 3, 2).await?;
        let first_dealings = deal_for_all(&first_root, &first)?;

        let registrations = first
            .identities
            .iter()
            .map(|identity| identity.join(REGISTRATION_FILE))
            .collect::<Vec<_>>();
        let second_ceremony = second_root.join("ceremony");
        prepare(&first.genesis.path, 2, &"44".repeat(32), &registrations, &second_ceremony)?;
        let second = TestCeremony {
            genesis: first.genesis.clone(),
            ceremony: second_ceremony,
            identities: first.identities.clone(),
        };
        let second_dealings = deal_for_all(&second_root, &second)?;
        let accepted =
            accept_for_all::<ShareOpeningBackend>(&second_root, &second, &second_dealings).await?;
        let output = second_root.join("bundle");
        let error = finalize::<ShareOpeningBackend>(
            &first.genesis.path,
            &second.ceremony,
            &first.identities[0].join(IDENTITY_SECRET_FILE),
            &first_dealings[0].join(PRIVATE_STATE_FILE),
            &dealing_paths(&second_dealings, DECRYPTION_DEALING_FILE),
            &dealing_paths(&second_dealings, CONTEXT_DEALING_FILE),
            &accepted.transcript,
            &accepted.acceptances,
            &output,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("another ceremony"));
        assert!(!output.exists());
        Ok(())
    }

    #[tokio::test]
    async fn deal_rejects_unknown_identity_and_existing_output() -> TestResult {
        let root = tempfile::tempdir()?;
        let ceremony = prepare_test_ceremony(root.path(), 3, 2).await?;
        let outsider = root.path().join("outsider.wire");
        fs_err::write(&outsider, encode_identity_secret(&StorageScalar::random(&mut OsRng)))?;
        let output = root.path().join("deal");
        let mut rng = ChaCha20Rng::from_seed([43; 32]);
        assert!(
            deal::<ShareOpeningBackend>(
                &ceremony.genesis.path,
                &ceremony.ceremony,
                &outsider,
                &output,
                &mut rng,
            )
            .is_err()
        );
        assert!(!output.exists());

        fs_err::create_dir(&output)?;
        assert!(
            deal::<ShareOpeningBackend>(
                &ceremony.genesis.path,
                &ceremony.ceremony,
                &ceremony.identities[0].join(IDENTITY_SECRET_FILE),
                &output,
                &mut rng,
            )
            .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "slow: runs the concrete Secp/Secq proof backend"]
    async fn paper_backend_completes_two_round_ceremony() -> TestResult {
        let root = tempfile::tempdir()?;
        let ceremony = prepare_test_ceremony(root.path(), 2, 2).await?;
        let mut rng = ChaCha20Rng::from_seed([44; 32]);
        let mut dealings = Vec::new();
        for (position, identity) in ceremony.identities.iter().enumerate() {
            let output = root.path().join(format!("paper-deal-{position}"));
            deal::<SecpSecqBackend>(
                &ceremony.genesis.path,
                &ceremony.ceremony,
                &identity.join(IDENTITY_SECRET_FILE),
                &output,
                &mut rng,
            )?;
            dealings.push(output);
        }
        let accepted = accept_for_all::<SecpSecqBackend>(root.path(), &ceremony, &dealings).await?;
        for position in 0..2 {
            let output = root.path().join(format!("paper-bundle-{position}"));
            finalize::<SecpSecqBackend>(
                &ceremony.genesis.path,
                &ceremony.ceremony,
                &ceremony.identities[position].join(IDENTITY_SECRET_FILE),
                &dealings[position].join(PRIVATE_STATE_FILE),
                &dealing_paths(&dealings, DECRYPTION_DEALING_FILE),
                &dealing_paths(&dealings, CONTEXT_DEALING_FILE),
                &accepted.transcript,
                &accepted.acceptances,
                &output,
            )?;
        }
        Ok(())
    }
}
