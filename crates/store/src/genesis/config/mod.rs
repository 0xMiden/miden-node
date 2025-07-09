//! Describe a subset of the genesis manifest in easily human readable format

use std::collections::HashMap;

use miden_lib::{
    AuthScheme,
    account::{auth::RpoFalcon512, faucets::BasicFungibleFaucet, wallets::create_basic_wallet},
    transaction::memory,
};
use miden_node_utils::crypto::get_rpo_random_coin;
use miden_objects::{
    Felt, FieldElement, ONE, Word, ZERO,
    account::{
        Account, AccountBuilder, AccountDelta, AccountFile, AccountId, AccountStorageDelta,
        AccountStorageMode, AccountType, AccountVaultDelta, AuthSecretKey, FungibleAssetDelta,
        NonFungibleAssetDelta,
    },
    asset::{FungibleAsset, TokenSymbol},
    crypto::dsa::rpo_falcon512::SecretKey,
};
use rand::{Rng, SeedableRng, distr::weighted::Weight};
use rand_chacha::ChaCha20Rng;

use crate::GenesisState;

mod errors;
use self::errors::GenesisConfigError;

#[cfg(test)]
mod tests;

// GENESIS CONFIG
// ================================================================================================

/// Specify a set of faucets and wallets with assets for easier test deployments.
///
/// Notice: Any faucet must be declared _before_ it's use in a wallet/regular account.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GenesisConfig {
    version: u32,
    timestamp: u32,
    wallet: Vec<WalletConfig>,
    fungible_faucet: Vec<FungibleFaucetConfig>,
}

impl Default for GenesisConfig {
    fn default() -> Self {
        Self {
            version: 1_u32,
            timestamp: u32::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("Time does not go backwards")
                    .as_secs(),
            )
            .expect("Timestamp should fit into u32"),
            wallet: vec![],
            fungible_faucet: vec![FungibleFaucetConfig {
                max_supply: 100_000_000_000u64,
                decimals: 6u8,
                storage_mode: StorageMode::Public,
                symbol: "MIDEN".to_owned(),
            }],
        }
    }
}

impl GenesisConfig {
    /// Read the genesis accounts from a toml formatted string
    ///
    /// Notice: It will generate the specified case during [`fn into_state`].
    pub fn read_toml(toml_str: &str) -> Result<Self, GenesisConfigError> {
        let me = toml::from_str::<Self>(toml_str)?;
        Ok(me)
    }

    /// Convert the in memory representation into the new genesis state
    ///
    /// Also returns the set of secrets for the generated accounts.
    #[allow(clippy::too_many_lines)]
    pub fn into_state(self) -> Result<(GenesisState, AccountSecrets), GenesisConfigError> {
        let GenesisConfig {
            version,
            timestamp,
            fungible_faucet: fungible_faucet_configs,
            wallet: wallet_configs,
        } = self;

        let mut wallet_accounts = Vec::<Account>::new();
        // Every asset sitting in a wallet, has to reference a faucet for that asset
        let mut faucet_accounts = HashMap::<String, Account>::new();

        // Collect the generated secret keys for the test, so one can interact with those
        // accounts/sign transactions
        let mut secrets = Vec::new();

        // First setup all the faucets
        for FungibleFaucetConfig {
            symbol,
            decimals,
            max_supply,
            storage_mode,
        } in fungible_faucet_configs
        {
            let mut rng = ChaCha20Rng::from_seed(rand::random());
            let secret_key = SecretKey::with_rng(&mut get_rpo_random_coin(&mut rng));
            let auth = RpoFalcon512::new(secret_key.public_key());
            let init_seed: [u8; 32] = rng.random();

            let token_symbol = TokenSymbol::new(&symbol)?;

            let account_type = AccountType::FungibleFaucet;

            let max_supply = Felt::try_from(max_supply)
                .expect("The `Felt::MODULUS` is _always_ larger than the `max_supply`");

            let component = BasicFungibleFaucet::new(token_symbol, decimals, max_supply)?;

            let account_storage_mode = storage_mode.into();

            // It's similar to `fn create_basic_fungible_faucet`, but we need to cover more cases.
            let (faucet_account, faucet_account_seed) = AccountBuilder::new(init_seed)
                .account_type(account_type)
                .storage_mode(account_storage_mode)
                .with_auth_component(auth)
                .with_component(component)
                .build()?;

            debug_assert_eq!(faucet_account.nonce(), Felt::ZERO);

            if faucet_accounts.insert(symbol.clone(), faucet_account.clone()).is_some() {
                return Err(GenesisConfigError::DuplicateFaucetDefinition { symbol: token_symbol });
            }

            secrets.push((
                format!("faucet_{symbol}.mac", symbol = symbol.to_lowercase()),
                faucet_account.id(),
                secret_key,
                faucet_account_seed,
            ));

            // Do _not_ collect the account, only after we know all wallet assets
            // we know the remaining supply in the faucets.
        }

        // Track all adjustments, one per faucet account id
        let mut faucet_issuance = HashMap::<AccountId, u64>::new();

        let zero_padding_width = usize::ilog10(std::cmp::max(10, wallet_configs.len())) as usize;

        // Setup all wallet accounts, which reference the faucet's for their provided assets.
        for (index, WalletConfig { has_updatable_code, storage_mode, assets }) in
            wallet_configs.into_iter().enumerate()
        {
            tracing::debug!("Adding wallet account {index} with {assets:?}");

            let mut rng = ChaCha20Rng::from_seed(rand::random());
            let secret_key = SecretKey::with_rng(&mut get_rpo_random_coin(&mut rng));
            let auth = AuthScheme::RpoFalcon512 { pub_key: secret_key.public_key() };
            let init_seed: [u8; 32] = rng.random();

            let account_type = if has_updatable_code {
                AccountType::RegularAccountUpdatableCode
            } else {
                AccountType::RegularAccountImmutableCode
            };
            let account_storage_mode = storage_mode.into();
            let (mut wallet_account, wallet_account_seed) =
                create_basic_wallet(init_seed, auth, account_type, account_storage_mode)?;

            // Add fungible assets and track the faucet adjustments per faucet/asset.
            let wallet_fungible_asset_update =
                prepare_fungible_asset_update(assets, &faucet_accounts, &mut faucet_issuance)?;

            // Force the account nonce to 1.
            //
            // By convention, a nonce of zero indicates a freshly generated local account that has
            // yet to be deployed. An account is deployed onchain along with its first
            // transaction which results in a non-zero nonce onchain.
            //
            // The genesis block is special in that accounts are "deplyed" without transactions and
            // therefore we need bump the nonce manually to uphold this invariant.
            let wallet_delta = AccountDelta::new(
                wallet_account.id(),
                AccountStorageDelta::default(),
                AccountVaultDelta::new(
                    wallet_fungible_asset_update,
                    NonFungibleAssetDelta::default(),
                ),
                ONE,
            )?;

            wallet_account.apply_delta(&wallet_delta)?;

            debug_assert_eq!(wallet_account.nonce(), ONE);

            secrets.push((
                format!("wallet_{index:0zero_padding_width$}.mac"),
                wallet_account.id(),
                secret_key,
                wallet_account_seed,
            ));

            wallet_accounts.push(wallet_account);
        }

        let mut all_accounts = Vec::<Account>::new();
        // Apply all fungible faucet adjustments to the respective faucet
        for (symbol, mut faucet_account) in faucet_accounts {
            let faucet_id = faucet_account.id();
            // If there is no account using the asset, we use an empty delta to set the
            // nonce to `ONE`.
            let total_issuance = faucet_issuance.get(&faucet_id).copied().unwrap_or_default();

            let mut storage_delta = AccountStorageDelta::default();

            if total_issuance != 0 {
                // slot 0
                storage_delta.set_item(
                    memory::FAUCET_STORAGE_DATA_SLOT,
                    [ZERO, ZERO, ZERO, Felt::new(total_issuance)],
                );
                tracing::debug!(
                    "Reducing faucet account {faucet} for {symbol} by {amount}",
                    faucet = faucet_id.to_hex(),
                    symbol = symbol,
                    amount = total_issuance
                );
            } else {
                tracing::debug!(
                    "No wallet is referencing {faucet} for {symbol}",
                    faucet = faucet_id.to_hex(),
                    symbol = symbol,
                );
            }

            faucet_account.apply_delta(&AccountDelta::new(
                faucet_id,
                storage_delta,
                AccountVaultDelta::default(),
                ONE,
            )?)?;

            debug_assert_eq!(faucet_account.nonce(), ONE);

            // sanity check the total issuance against
            let basic = BasicFungibleFaucet::try_from(&faucet_account)?;
            let max_supply = basic.max_supply().inner();
            if max_supply < total_issuance {
                return Err(GenesisConfigError::MaxIssuanceExceeded {
                    max_supply,
                    symbol: TokenSymbol::new(&symbol)?,
                    total_issuance,
                });
            }

            all_accounts.push(faucet_account);
        }
        // Ensure the faucets always precede the wallets referencing them
        all_accounts.extend(wallet_accounts);

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

// FUNGIBLE FAUCET CONFIG
// ================================================================================================

/// Represents a faucet with asset specific properties
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FungibleFaucetConfig {
    // TODO eventually directly parse to `TokenSymbol`
    symbol: String,
    decimals: u8,
    /// Max supply in full token units
    ///
    /// It will be converted internally to the smallest representable unit,
    /// using based `10.powi(decimals)` as a multiplier.
    max_supply: u64,
    #[serde(default)]
    storage_mode: StorageMode,
}

// WALLET CONFIG
// ================================================================================================

/// Represents a wallet, containing a set of assets
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WalletConfig {
    #[serde(default)]
    has_updatable_code: bool,
    #[serde(default)]
    storage_mode: StorageMode,
    assets: Vec<AssetEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct AssetEntry {
    symbol: String,
    /// The amount of full token units the given asset is populated with
    amount: u64,
}

// STORAGE MODE
// ================================================================================================

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

// ACCOUNTS
// ================================================================================================

#[derive(Debug, Clone)]
pub struct AccountFileWithName {
    pub name: String,
    pub account_file: AccountFile,
}

/// Secrets generated during the state generation
#[derive(Debug, Clone)]
pub struct AccountSecrets {
    // name, account, private key, account seed
    pub secrets: Vec<(String, AccountId, SecretKey, Word)>,
}

impl AccountSecrets {
    /// Convert the internal tuple into an `AccountFile`
    ///
    /// If no name is present, a new one is generated based on the current time
    /// and the index in
    pub fn as_account_files(
        &self,
        genesis_state: &GenesisState,
    ) -> impl Iterator<Item = Result<AccountFileWithName, GenesisConfigError>> + use<'_> {
        let account_lut = HashMap::<AccountId, Account>::from_iter(
            genesis_state.accounts.iter().map(|account| (account.id(), account.clone())),
        );
        self.secrets.iter().map(move |(name, account_id, secret_key, account_seed)| {
            let account = account_lut
                .get(account_id)
                .ok_or(GenesisConfigError::MissingGenesisAccount { account_id: *account_id })?;
            let account_file = AccountFile::new(
                account.clone(),
                Some(*account_seed),
                vec![AuthSecretKey::RpoFalcon512(secret_key.clone())],
            );
            let name = name.to_string();
            Ok(AccountFileWithName { name, account_file })
        })
    }
}

// HELPERS
// ================================================================================================

/// Process wallet assets and return them as a fungible asset delta.
/// Track the negative adjustments for the respective faucets.
fn prepare_fungible_asset_update(
    assets: impl IntoIterator<Item = AssetEntry>,
    faucets: &HashMap<String, Account>,
    faucet_issuance: &mut HashMap<AccountId, u64>,
) -> Result<FungibleAssetDelta, GenesisConfigError> {
    let assets =
        Result::<Vec<_>, _>::from_iter(assets.into_iter().map(|AssetEntry { amount, symbol }| {
            let token_symbol = TokenSymbol::new(&symbol)?;
            let faucet_account = faucets.get(&symbol).ok_or_else(|| {
                GenesisConfigError::MissingFaucetDefinition { symbol: token_symbol }
            })?;

            Ok::<_, GenesisConfigError>(FungibleAsset::new(faucet_account.id(), amount)?)
        }))?;

    let mut wallet_asset_delta = FungibleAssetDelta::default();
    assets
        .into_iter()
        .try_for_each(|fungible_asset| wallet_asset_delta.add(fungible_asset))?;

    wallet_asset_delta.iter().try_for_each(|(faucet_id, amount)| {
        let issuance: &mut u64 = faucet_issuance.entry(*faucet_id).or_default();
        tracing::debug!(
            "Updating faucet issuance {faucet} with {issuance} += {amount}",
            faucet = faucet_id.to_hex()
        );

        // check against total supply is deferred
        issuance
            .checked_add_assign(
                &u64::try_from(*amount)
                    .expect("Issuance must always be positive in the scope of genesis config"),
            )
            .map_err(|_| GenesisConfigError::IssuanceOverflow)?;

        Ok::<_, GenesisConfigError>(())
    })?;

    Ok(wallet_asset_delta)
}
