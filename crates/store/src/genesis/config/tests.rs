use assert_matches::assert_matches;
use miden_lib::transaction::memory;
use miden_objects::ONE;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
#[miden_node_test_macro::enable_logging]
fn parsing_yields_expected_default_values() -> TestResult {
    let s = include_str!("./samples/01-simple.toml");
    let gcfg = GenesisConfig::read_toml(s)?;
    let (state, _secrets) = gcfg.into_state()?;
    let _ = state;
    // faucets always precede wallet accounts
    let faucet = state.accounts[0].clone();
    let wallet1 = state.accounts[1].clone();
    let wallet2 = state.accounts[2].clone();

    assert!(faucet.is_faucet());
    assert!(wallet1.is_regular_account());
    assert!(wallet2.is_regular_account());

    assert_eq!(faucet.nonce(), ONE);
    assert_eq!(wallet1.nonce(), ONE);
    assert_eq!(wallet2.nonce(), ONE);

    {
        let faucet = BasicFungibleFaucet::try_from(faucet.clone()).unwrap();

        assert_eq!(faucet.max_supply(), Felt::new(100_000_000));
        assert_eq!(faucet.decimals(), 3);
        assert_eq!(faucet.symbol(), TokenSymbol::new("MIDEN").unwrap());
    }

    // check account balance, and ensure ordering is retained
    assert_matches!(wallet1.vault().get_balance(faucet.id()), Ok(val) => {
        assert_eq!(val, 999_000);
    });
    assert_matches!(wallet2.vault().get_balance(faucet.id()), Ok(val) => {
        assert_eq!(val, 777);
    });

    // check total issuance of the faucet
    assert_eq!(
        faucet.storage().get_item(memory::FAUCET_STORAGE_DATA_SLOT).unwrap()[3],
        Felt::new(999_777),
        "Issuance mismatch"
    );

    Ok(())
}

#[test]
#[miden_node_test_macro::enable_logging]
fn genesis_accounts_have_nonce_one() -> TestResult {
    let gcfg = GenesisConfig::default();
    let (state, secrets) = gcfg.into_state().unwrap();
    let mut iter = secrets.as_account_files(&state);
    let AccountFileWithName { account_file: status_quo, .. } = iter.next().unwrap().unwrap();
    assert!(iter.next().is_none());

    assert_eq!(status_quo.account.nonce(), ONE);

    let _block = state.into_block()?;
    Ok(())
}
