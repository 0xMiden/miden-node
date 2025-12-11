use assert_matches::assert_matches;
use fs_err;
use miden_lib::utils::Deserializable;
use miden_objects::ONE;
use miden_objects::crypto::dsa::ecdsa_k256_keccak::SecretKey;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
#[miden_node_test_macro::enable_logging]
fn parsing_yields_expected_default_values() -> TestResult {
    let s = include_str!("./samples/01-simple.toml");
    let gcfg = GenesisConfig::read_toml(s)?;
    let (state, _secrets) = gcfg.into_state(SecretKey::new())?;
    let _ = state;
    // faucets always precede wallet accounts
    let native_faucet = state.accounts[0].clone();
    let _excess = state.accounts[1].clone();
    let wallet1 = state.accounts[2].clone();
    let wallet2 = state.accounts[3].clone();

    assert!(native_faucet.is_faucet());
    assert!(wallet1.is_regular_account());
    assert!(wallet2.is_regular_account());

    assert_eq!(native_faucet.nonce(), ONE);
    assert_eq!(wallet1.nonce(), ONE);
    assert_eq!(wallet2.nonce(), ONE);

    {
        let faucet = BasicFungibleFaucet::try_from(native_faucet.clone()).unwrap();

        assert_eq!(faucet.max_supply(), Felt::new(100_000_000));
        assert_eq!(faucet.decimals(), 3);
        assert_eq!(faucet.symbol(), TokenSymbol::new("MIDEN").unwrap());
    }

    // check account balance, and ensure ordering is retained
    assert_matches!(wallet1.vault().get_balance(native_faucet.id()), Ok(val) => {
        assert_eq!(val, 999_000);
    });
    assert_matches!(wallet2.vault().get_balance(native_faucet.id()), Ok(val) => {
        assert_eq!(val, 777);
    });

    // check total issuance of the faucet
    assert_eq!(
        native_faucet
            .storage()
            .get_item(AccountStorage::faucet_metadata_slot())
            .unwrap()[3],
        Felt::new(999_777),
        "Issuance mismatch"
    );

    Ok(())
}

#[test]
#[miden_node_test_macro::enable_logging]
fn genesis_accounts_have_nonce_one() -> TestResult {
    let gcfg = GenesisConfig::default();
    let (state, secrets) = gcfg.into_state(SecretKey::new()).unwrap();
    let mut iter = secrets.as_account_files(&state);
    let AccountFileWithName { account_file: status_quo, .. } = iter.next().unwrap().unwrap();
    assert!(iter.next().is_none());

    assert_eq!(status_quo.account.nonce(), ONE);

    let _block = state.into_block()?;
    Ok(())
}

#[test]
#[miden_node_test_macro::enable_logging]
fn insecure_signer_creation() -> TestResult {
    // Default config.
    let default_gcfg = GenesisConfig::default();
    // Config from toml.
    let s = include_str!("./samples/01-simple.toml");
    let toml_gcfg = GenesisConfig::read_toml(s)?;

    // Test both default and from toml.
    for (gcfg, filepath) in vec![
        (default_gcfg, GenesisConfig::DEFAULT_INSECURE_KEY_FILEPATH),
        (toml_gcfg, "/tmp/insecure_2"),
    ] {
        let _signer = gcfg.signer_config.signer();

        // Read the secret key from the file that was created by the genesis config.
        let file_bytes = fs_err::read(filepath)?;
        let read_signer = SecretKey::read_from_bytes(&file_bytes)?;

        // Verify that we can successfully read a valid secret key from the file.
        assert_eq!(read_signer.to_bytes().len(), 32); // ECDSA K256 secret keys are 32 bytes.

        // Verify that the secret key is non-zero.
        assert_ne!(read_signer.to_bytes(), [0u8; 32]);

        // Verify we can create a new SecretKey from the same bytes.
        let round_trip_signer = SecretKey::read_from_bytes(&read_signer.to_bytes())?;
        assert_eq!(read_signer.to_bytes(), round_trip_signer.to_bytes());

        // Clean up the test file.
        fs_err::remove_file(filepath).ok();
    }
    Ok(())
}
