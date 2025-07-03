use miden_lib::account::faucets::FungibleFaucetError;
use miden_objects::{AccountError, AssetError, TokenSymbolError, asset::TokenSymbol};

#[allow(missing_docs, reason = "Error variants must be descriptive by themselves")]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Toml(#[from] toml::de::Error),
    #[error("Account translation from config to state failed")]
    Account(#[from] AccountError),
    #[error("Asset translation from config to state failed")]
    Asset(#[from] AssetError),
    #[error("Applying assets to account failed")]
    AccountDelta(#[from] miden_objects::AccountDeltaError),
    #[error("The defined asset {symbol:?} has no corresponding faucet")]
    MissingFaucetDefinition { symbol: TokenSymbol },
    #[error(transparent)]
    TokenSymbol(#[from] TokenSymbolError),
    #[error("The provided max supply {max_supply} exceeds the field modulus {modulus}")]
    MaxSupplyExceedsFieldModulus { max_supply: u64, modulus: u64 },
    #[error("Unsupported value for key {key} : {value}")]
    UnsupportedValue {
        key: &'static str,
        value: String,
        message: String,
    },
    #[error("Failed to create fungible faucet account")]
    FungibleFaucet(#[from] FungibleFaucetError),
}
