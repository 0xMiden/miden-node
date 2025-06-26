#![allow(dead_code)]
#![allow(clippy::from_iter_instead_of_collect)]

//! Describe a subset of the genesis manifest in easily human readable format

use std::collections::HashMap;

use miden_lib::{
    AuthScheme,
    account::{auth::RpoFalcon512, faucets::BasicFungibleFaucet, wallets::create_basic_wallet},
    utils::{self},
};
use miden_node_utils::crypto::get_rpo_random_coin;
use miden_objects::{
    AccountError, AssetError, Felt, FieldElement, StarkField, TokenSymbolError,
    account::{
        AccountBuilder, AccountDelta, AccountId, AccountIdAnchor, AccountStorageDelta,
        AccountStorageMode, AccountType, AccountVaultDelta,
    },
    asset::{Asset, FungibleAsset, TokenSymbol},
    crypto::dsa::rpo_falcon512::SecretKey,
};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;

use crate::GenesisState;

/// Represents an account, either a wallet or a faucet
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AccountRepr {
    Wallet(WalletRepr),
    Faucet(FaucetRepr),
}

/// Represents a wallet, containing a set of assets
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WalletRepr {
    can_be_updated: bool,
    #[serde(default)]
    storage_mode: StorageMode,
    assets: Vec<AssetEntry>,
}

/// `false` doesn't pass the `syn::Path` parsing, so we do one level indirection
const fn nein() -> bool {
    false
}

/// Represents a faucet with asset specific properties
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FaucetRepr {
    // TODO eventually directly parse to `TokenSymbol`
    symbol: String,
    decimals: u8,
    max_supply: u64,
    #[serde(default)]
    storage_mode: StorageMode,
    #[serde(default = "self::nein")]
    fungible: bool,
}

/// See the [full description](https://0xmiden.github.io/miden-base/account.html?highlight=Accoun#account-storage-mode)
/// for details
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, Default)]
pub enum StorageMode {
    /// Monitor for `Notes` related to the account, in addition to being `Public`.
    #[serde(alias = "network")]
    #[default]
    Network,
    /// A publicly stored account, lives on-chain.
    #[serde(alias = "pub")]
    Public,
    /// A private account, which must be known by interactors.
    #[serde(alias = "private")]
    Private,
}

impl From<StorageMode> for AccountStorageMode {
    fn from(mode: StorageMode) -> AccountStorageMode {
        match mode {
            StorageMode::Network => AccountStorageMode::Network,
            StorageMode::Private => AccountStorageMode::Private,
            StorageMode::Public => AccountStorageMode::Public,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AssetKind {
    Fungible,
    NonFungible,
}

#[allow(missing_docs, reason = "Error variants must be descriptive by themselves")]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Toml(#[from] toml::de::Error),
    #[error("Failed to load library source from path")]
    LibraryReference(#[source] std::io::Error),
    #[error("Failed to and deserialize library, assuming it uses `Deserializable` file format")]
    LibraryOnDiskFormat(#[source] utils::DeserializationError),
    #[error("Account translation from config to state failed")]
    Account(#[from] AccountError),
    #[error("Asset translation from config to state failed")]
    Asset(#[from] AssetError),
    #[error("Applying assets to account failed")]
    AccountDelta(#[from] miden_objects::AccountDeltaError),
    #[error("You defined an asset {symbol} that has no faucet producing it")]
    MissingFaucetDefinition { symbol: String },
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
}

/// Secrets generated during the state generation
///
/// Attention: Only to be used for testing, for all other usecases
/// inject only the public keys.
#[derive(Debug, Clone)]
pub struct TestSecrets {
    pub secrets: Vec<(AccountType, AccountId, SecretKey)>,
}

/// Specify a set of faucets and wallets with assets for easier test depoyments
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TestGenesisConfig {
    version: u32,
    timestamp: u32,
    account: Vec<WalletRepr>,
    faucet: Vec<FaucetRepr>,
}

impl TestGenesisConfig {
    /// Read the genesis accounts from a toml formatted string
    ///
    /// Notice: It will generate the specified case during [`fn into_state`].
    pub fn read_toml(toml_str: &str) -> Result<Self, Error> {
        let me = toml::from_str::<Self>(toml_str)?;
        Ok(me)
    }

    /// Convert the in memory representation into the new genesis state
    ///
    /// Notice: Generates keys and returns the secret keys, hence this is not sane to be used
    /// for production environments. There, you want to generate the keys externally.
    pub fn into_state(self) -> Result<(GenesisState, TestSecrets), Error> {
        let version = self.version;
        let timestamp = self.timestamp;
        let repr_accounts = self.account;
        let repr_faucets = self.faucet;
        let mut all_accounts = Vec::new();

        // Every asset sitting in a wallet, has to reference a faucet for that asset
        let mut faucets = HashMap::<String, AccountId>::new();

        // Collect the generated secret keys for the test, so one can interact with those
        // accounts/sign transactions
        let mut secrets = Vec::<(AccountType, AccountId, SecretKey)>::new();

        let anchor = AccountIdAnchor::PRE_GENESIS;
        // first setup all the faucets
        for FaucetRepr {
            symbol,
            decimals,
            max_supply,
            storage_mode,
            fungible,
        } in repr_faucets
        {
            let mut rng = ChaCha20Rng::from_seed(rand::random());
            let secret = SecretKey::with_rng(&mut get_rpo_random_coin(&mut rng));
            let auth = RpoFalcon512::new(secret.public_key());
            let init_seed: [u8; 32] = rng.random();

            let token_symbol = TokenSymbol::new(&symbol)?;
            let max_supply = Felt::try_from(max_supply).map_err(|_| {
                Error::MaxSupplyExceedsFieldModulus { max_supply, modulus: Felt::MODULUS }
            })?;

            let account_type = if fungible {
                AccountType::FungibleFaucet
            } else {
                AccountType::NonFungibleFaucet
            };

            if !fungible {
                return Err(Error::UnsupportedValue {
                    key: "fungible",
                    value: false.to_string(),
                    message: "Not supported just yet".to_owned(),
                });
            }

            let component = BasicFungibleFaucet::new(token_symbol, decimals, max_supply)
                .map_err(AccountError::FungibleFaucetError)?;

            let account_storage_mode = storage_mode.into();

            // similar to `fn create_basic_fungible_faucet`, but we need to cover more cases
            let (faucet_account, _faucet_account_seed) = AccountBuilder::new(init_seed)
                .anchor(anchor)
                .account_type(account_type)
                .storage_mode(account_storage_mode)
                .with_component(auth)
                .with_component(component)
                .build()?;

            faucets.insert(symbol, faucet_account.id());

            secrets.push((account_type, faucet_account.id(), secret));

            all_accounts.push(faucet_account);
        }

        // then setup all wallet accounts, which reference the faucet's for their provided assets
        for WalletRepr { can_be_updated, storage_mode, assets } in repr_accounts {
            let mut rng = ChaCha20Rng::from_seed(rand::random());
            let secret = SecretKey::with_rng(&mut get_rpo_random_coin(&mut rng));
            let auth = AuthScheme::RpoFalcon512 { pub_key: secret.public_key() };
            let init_seed: [u8; 32] = rng.random();

            let assets = Result::<Vec<_>, Error>::from_iter(assets.into_iter().map(
                |AssetEntry { amount, symbol }: AssetEntry| {
                    let _ = TokenSymbol::new(&symbol)?;
                    let faucet_id = faucets
                        .get(&symbol)
                        .ok_or_else(|| Error::MissingFaucetDefinition { symbol: symbol.clone() })?;
                    // FIXME TODO add non funcgible assets
                    Ok(Asset::Fungible(FungibleAsset::new(*faucet_id, amount)?))
                },
            ))?;
            let account_type = if can_be_updated {
                AccountType::RegularAccountUpdatableCode
            } else {
                AccountType::RegularAccountImmutableCode
            };
            let account_storage_mode = storage_mode.into();
            let (mut account, _seed) =
                create_basic_wallet(init_seed, anchor, auth, account_type, account_storage_mode)?;
            // by convention, 1 is the nonce for a shared account, which genesis by definition
            // is, so all the accounts there should have nonce 1
            let delta = AccountDelta::new(
                AccountStorageDelta::default(),
                AccountVaultDelta::from_iters(assets, None),
                Some(Felt::ONE),
            )?;
            account.apply_delta(&delta)?;

            secrets.push((account_type, account.id(), secret));

            all_accounts.push(account);
        }

        Ok((
            GenesisState {
                accounts: all_accounts,
                version,
                timestamp,
            },
            TestSecrets { secrets },
        ))
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct AssetEntry {
    symbol: String,
    amount: u64, // FIXME denominators are in order
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_01_works() -> Result<(), Box<dyn std::error::Error>> {
        let s = include_str!("./samples/01-simple.toml");
        let gcfg = TestGenesisConfig::read_toml(s)?;
        dbg!(&gcfg);
        let state = gcfg.into_state()?;
        let _ = dbg!(state);
        Ok(())
    }
}
