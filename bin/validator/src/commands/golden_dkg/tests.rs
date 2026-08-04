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

#[test]
fn committed_fixture_has_one_valid_share_per_participant() -> TestResult {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/testdata/insecure-golden-storage-key");
    let root = tempfile::tempdir()?;
    let mut shares = Vec::new();

    for participant in 1..=3 {
        let bundle = root.path().join(format!("validator-{participant}"));
        fs_err::create_dir(&bundle)?;
        fs_err::write(bundle.join(EPOCH_FILE), "09".repeat(32))?;
        fs_err::copy(fixture.join(SETUP_CONTEXT_FILE), bundle.join(SETUP_CONTEXT_FILE))?;
        fs_err::copy(fixture.join(PUBLIC_KEY_SET_FILE), bundle.join(PUBLIC_KEY_SET_FILE))?;
        fs_err::copy(
            fixture.join(format!("validator-{participant}/{SECRET_SHARE_FILE}")),
            bundle.join(SECRET_SHARE_FILE),
        )?;
        validate_fixture_bundle(&bundle, participant)?;
        shares.push(fs_err::read(bundle.join(SECRET_SHARE_FILE))?);
    }

    assert_ne!(shares[0], shares[1]);
    assert_ne!(shares[1], shares[2]);
    assert_ne!(shares[0], shares[2]);
    Ok(())
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
    super::super::genesis::generate(&genesis_directory, &accounts_directory, Some(&config_path))?;
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

    let manifest: Manifest = toml::from_str(&fs_err::read_to_string(output.join(MANIFEST_FILE))?)?;
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
    deal_for_all_with_seed(root, ceremony, [41; 32])
}

/// Creates both dealings using one deterministic test seed.
fn deal_for_all_with_seed(
    root: &Path,
    ceremony: &TestCeremony,
    seed: [u8; 32],
) -> TestResultWith<Vec<PathBuf>> {
    let mut rng = ChaCha20Rng::from_seed(seed);
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
        acceptances: outputs.iter().map(|output| output.join(TRANSCRIPT_ACCEPTANCE_FILE)).collect(),
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
    let accepted = accept_for_all::<ShareOpeningBackend>(root.path(), &ceremony, &dealings).await?;
    let mut bundles = Vec::new();
    for position in 0..3 {
        let bundle = finalize_test_bundle(root.path(), &ceremony, &dealings, &accepted, position)?;
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
async fn validate_rejects_an_internally_consistent_substitute_key_set() -> TestResult {
    let root = tempfile::tempdir()?;
    let alternate_root = root.path().join("alternate");
    fs_err::create_dir(&alternate_root)?;
    let ceremony = prepare_test_ceremony(root.path(), 3, 2).await?;

    let dealings = deal_for_all(root.path(), &ceremony)?;
    let accepted = accept_for_all::<ShareOpeningBackend>(root.path(), &ceremony, &dealings).await?;
    let bundle = finalize_test_bundle(root.path(), &ceremony, &dealings, &accepted, 0)?;

    let alternate_dealings = deal_for_all_with_seed(&alternate_root, &ceremony, [77; 32])?;
    let alternate_accepted =
        accept_for_all::<ShareOpeningBackend>(&alternate_root, &ceremony, &alternate_dealings)
            .await?;
    let alternate_bundle = finalize_test_bundle(
        &alternate_root,
        &ceremony,
        &alternate_dealings,
        &alternate_accepted,
        0,
    )?;
    fs_err::copy(alternate_bundle.join(PUBLIC_KEY_SET_FILE), bundle.join(PUBLIC_KEY_SET_FILE))?;
    fs_err::copy(alternate_bundle.join(SECRET_SHARE_FILE), bundle.join(SECRET_SHARE_FILE))?;

    let error = validate_bundle(
        &ceremony.genesis.path,
        &ceremony.ceremony,
        &hex::encode(ceremony.genesis.signing_keys[0].public_key().to_bytes()),
        &bundle,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("public key set"));
    Ok(())
}

#[tokio::test]
async fn finalize_rejects_incomplete_or_duplicate_dealings() -> TestResult {
    let root = tempfile::tempdir()?;
    let ceremony = prepare_test_ceremony(root.path(), 3, 2).await?;
    let dealings = deal_for_all(root.path(), &ceremony)?;
    let accepted = accept_for_all::<ShareOpeningBackend>(root.path(), &ceremony, &dealings).await?;
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
    let accepted = accept_for_all::<ShareOpeningBackend>(root.path(), &ceremony, &dealings).await?;
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
    let accepted = accept_for_all::<ShareOpeningBackend>(root.path(), &ceremony, &dealings).await?;

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
    let accepted = accept_for_all::<ShareOpeningBackend>(root.path(), &ceremony, &dealings).await?;
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
    let accepted = accept_for_all::<ShareOpeningBackend>(root.path(), &ceremony, &dealings).await?;
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
