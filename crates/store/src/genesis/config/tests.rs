use super::*;

#[test]
fn parsing_yields_expected_default_values() -> Result<(), Box<dyn std::error::Error>> {
    let s = include_str!("./samples/01-simple.toml");
    let gcfg = TestGenesisConfig::read_toml(s)?;
    let (state, _secrets) = gcfg.into_state()?;
    let _ = state;
    assert!(state.accounts[0].is_faucet());
    assert!(state.accounts[1].is_regular_account());

    assert_matches::assert_matches!(state.accounts[1].vault().get_balance(state.accounts[0].id()), Ok(val) => {
        assert_eq!(val, 999);
    });
    Ok(())
}
