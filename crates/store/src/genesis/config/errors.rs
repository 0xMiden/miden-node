use miden_lib::account::faucets::FungibleFaucetError;
use miden_lib::account::wallets::{BasicWallet, BasicWalletError};
use miden_objects::account::AccountId;
use miden_objects::asset::TokenSymbol;
use miden_objects::{AccountError, AssetError, TokenSymbolError};

#[allow(missing_docs, reason = "Error variants must be descriptive by themselves")]
#[derive(Debug, thiserror::Error)]
pub enum GenesisConfigError {
    #[error(transparent)]
    Toml(#[from] toml::de::Error),
    #[error("account translation from config to state failed")]
    Account(#[from] AccountError),
    #[error("asset translation from config to state failed")]
    Asset(#[from] AssetError),
    #[error("adding assets to account failed")]
    AccountDelta(#[from] miden_objects::AccountDeltaError),
    #[error("the defined asset {symbol:?} has no corresponding faucet")]
    MissingFaucetDefinition { symbol: TokenSymbol },
    #[error("account with id {account_id} was referenced but is not part of given genesis state")]
    MissingGenesisAccount { account_id: AccountId },
    #[error(transparent)]
    TokenSymbol(#[from] TokenSymbolError),
    #[error("unsupported value for key {key} : {value}")]
    UnsupportedValue {
        key: &'static str,
        value: String,
        message: String,
    },
    #[error("failed to create fungible faucet account")]
    FungibleFaucet(#[from] FungibleFaucetError),
    #[error("failed to create basic wallet account")]
    BasicWallet(#[from] BasicWalletError),
    #[error(r#"incompatible combination of `max_supply` ({max_supply})" and `decimals` ({decimals}) exceeding the allowed value range of an `u64`"#)]
    OutOfRange { max_supply: u64, decimals: u8 },
    #[error("Found duplicate faucet definition for token symbol {symbol:?}")]
    DuplicateFaucetDefinition { symbol: TokenSymbol },
    #[error(
        "Total issuance {total_issuance} of {symbol:?} exceeds faucet's maximum issuance of {max_supply}"
    )]
    MaxIssuanceExceeded {
        symbol: TokenSymbol,
        total_issuance: u64,
        max_supply: u64,
    },
    #[error("Total issuance overflowed u64")]
    IssuanceOverflow,
}
