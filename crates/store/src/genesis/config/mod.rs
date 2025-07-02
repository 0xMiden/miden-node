//! Describe a subset of the genesis manifest in easily human readable format

use std::collections::HashMap;

use miden_lib::{
    AuthScheme,
    account::{
        auth::RpoFalcon512,
        faucets::{BasicFungibleFaucet, FungibleFaucetError},
        wallets::create_basic_wallet,
    },
};
use miden_node_utils::crypto::get_rpo_random_coin;
use miden_objects::{
    AccountError, AssetError, Felt, FieldElement, StarkField, TokenSymbolError, Word,
    account::{
        Account, AccountBuilder, AccountDelta, AccountFile, AccountId, AccountStorageDelta,
        AccountStorageMode, AccountType, AccountVaultDelta, AuthSecretKey, FungibleAssetDelta,
        NonFungibleAssetDelta,
    },
    asset::{FungibleAsset, TokenSymbol},
    crypto::dsa::rpo_falcon512::SecretKey,
};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;

use crate::GenesisState;

#[cfg(test)]
mod tests;

/// Represents an account, either a wallet or a faucet
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AccountConfig {
    Wallet(WalletConfig),
    Faucet(FaucetConfig),
}

/// Represents a wallet, containing a set of assets
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WalletConfig {
    #[serde(default)]
    can_be_updated: bool,
    #[serde(default)]
    storage_mode: StorageMode,
    assets: Vec<AssetEntry>,
}

/// `false` doesn't pass the `syn::Path` parsing, so we do one level indirection
const fn ja() -> bool {
    true
}

/// Represents a faucet with asset specific properties
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FaucetConfig {
    // TODO eventually directly parse to `TokenSymbol`
    symbol: String,
    decimals: u8,
    max_supply: u64,
    #[serde(default)]
    storage_mode: StorageMode,
    #[serde(default = "self::ja")]
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
    #[serde(alias = "public")]
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct AssetEntry {
    symbol: String, // TODO move to a wrapper around `TokenSymbol`
    amount: u64,    // TODO we might want to provide `humantime`-like denominations
}

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

/// Secrets generated during the state generation
#[derive(Debug, Clone)]
pub struct AccountSecrets {
    pub secrets: Vec<(Account, SecretKey, Word)>,
}

impl AccountSecrets {
    /// Convert the internal tuple into an `AccountFile`
    pub fn as_account_files(&self) -> impl Iterator<Item = AccountFile> {
        self.secrets.iter().map(|(account, secret_key, account_seed)| {
            AccountFile::new(
                account.clone(),
                Some(*account_seed),
                vec![AuthSecretKey::RpoFalcon512(secret_key.clone())],
            )
        })
    }
}

/// Specify a set of faucets and wallets with assets for easier test depoyments
///
/// Notice: Any faucet must be declared _before_ it's use in a wallet/regular account.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GenesisConfig {
    version: u32,
    timestamp: u32,
    wallet: Vec<WalletConfig>,
    faucet: Vec<FaucetConfig>,
}

impl GenesisConfig {
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
    pub fn into_state(self) -> Result<(GenesisState, AccountSecrets), Error> {
        let version = self.version;
        let timestamp = self.timestamp;
        let repr_accounts = self.wallet;
        let repr_faucets = self.faucet;
        let mut all_accounts = Vec::new();

        // Every asset sitting in a wallet, has to reference a faucet for that asset
        let mut faucets = HashMap::<String, AccountId>::new();

        // Collect the generated secret keys for the test, so one can interact with those
        // accounts/sign transactions
        let mut secrets = Vec::<(Account, SecretKey, Word)>::new();

        // First setup all the faucets
        for FaucetConfig {
            symbol,
            decimals,
            max_supply,
            storage_mode,
            fungible,
        } in repr_faucets
        {
            let mut rng = ChaCha20Rng::from_seed(rand::random());
            let secret_key = SecretKey::with_rng(&mut get_rpo_random_coin(&mut rng));
            let auth = RpoFalcon512::new(secret_key.public_key());
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

            let component = BasicFungibleFaucet::new(token_symbol, decimals, max_supply)?;

            let account_storage_mode = storage_mode.into();

            // It's similar to `fn create_basic_fungible_faucet`, but we need to cover more cases
            let (faucet_account, faucet_account_seed) = AccountBuilder::new(init_seed)
                .account_type(account_type)
                .storage_mode(account_storage_mode)
                .with_component(auth)
                .with_component(component)
                .build()?;

            faucets.insert(symbol, faucet_account.id());

            secrets.push((faucet_account.clone(), secret_key, faucet_account_seed));

            all_accounts.push(faucet_account);
        }

        // then setup all wallet accounts, which reference the faucet's for their provided assets
        for WalletConfig { can_be_updated, storage_mode, assets } in repr_accounts {
            let mut rng = ChaCha20Rng::from_seed(rand::random());
            let secret_key = SecretKey::with_rng(&mut get_rpo_random_coin(&mut rng));
            let auth = AuthScheme::RpoFalcon512 { pub_key: secret_key.public_key() };
            let init_seed: [u8; 32] = rng.random();

            let assets = Result::<Vec<_>, Error>::from_iter(assets.into_iter().map(
                |AssetEntry { amount, symbol }: AssetEntry| {
                    let token_symbol = TokenSymbol::new(&symbol)?;
                    let faucet_id = faucets
                        .get(&symbol)
                        .ok_or_else(|| Error::MissingFaucetDefinition { symbol: token_symbol })?;

                    Ok(FungibleAsset::new(*faucet_id, amount)?)
                },
            ))?;
            let account_type = if can_be_updated {
                AccountType::RegularAccountUpdatableCode
            } else {
                AccountType::RegularAccountImmutableCode
            };
            let account_storage_mode = storage_mode.into();
            let (mut wallet_account, wallet_account_seed) =
                create_basic_wallet(init_seed, auth, account_type, account_storage_mode)?;

            // Add fungible assets.
            let mut fungible_assets = FungibleAssetDelta::default();
            assets
                .into_iter()
                .try_for_each(|fungible_asset| fungible_assets.add(fungible_asset))?;

            let delta = AccountDelta::new(
                wallet_account.id(),
                AccountStorageDelta::default(),
                AccountVaultDelta::new(fungible_assets, NonFungibleAssetDelta::default()),
                Some(Felt::ONE),
            )?;
            wallet_account.apply_delta(&delta)?;

            secrets.push((wallet_account.clone(), secret_key, wallet_account_seed));

            all_accounts.push(wallet_account);
        }

        Ok((
            GenesisState {
                accounts: all_accounts,
                version,
                timestamp,
            },
            AccountSecrets { secrets },
        ))
    }
}
