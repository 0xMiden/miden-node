use miden_objects::ONE;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn parsing_yields_expected_default_values() -> TestResult {
    let s = include_str!("./samples/01-simple.toml");
    let gcfg = GenesisConfig::read_toml(s)?;
    let (state, _secrets) = gcfg.into_state()?;
    let _ = state;
    assert!(state.accounts[0].is_faucet());
    assert!(state.accounts[1].is_regular_account());

    assert_matches::assert_matches!(state.accounts[1].vault().get_balance(state.accounts[0].id()), Ok(val) => {
        assert_eq!(val, 999);
    });
    Ok(())
}

#[test]
fn generate_genesis_account_is_retains_behaviour() -> TestResult {
    let gcfg = GenesisConfig::default();
    let (state, secrets) = gcfg.into_state().unwrap();
    let mut iter = secrets.as_account_files();
    let AccountFileWithName { account_file: status_quo, .. } = iter.next().unwrap();
    assert!(iter.next().is_none());

    assert_eq!(status_quo.account.nonce(), ONE);

    let block = state.into_block()?;
    dbg!(block.inner().header());
    Ok(())
}
