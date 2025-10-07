//! Wallet account creation functionality.

use anyhow::Result;
use miden_lib::AuthScheme;
use miden_lib::account::wallets::create_basic_wallet;
use miden_node_utils::crypto::get_rpo_random_coin;
use miden_objects::account::{Account, AccountStorageMode, AccountType};
use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;

/// Create a wallet account with `RpoFalcon512` authentication.
///
/// Returns the created account and the secret key for authentication.
pub fn create_wallet_account() -> Result<(Account, SecretKey)> {
    let mut rng = ChaCha20Rng::from_seed(rand::random());
    let secret_key = SecretKey::with_rng(&mut get_rpo_random_coin(&mut rng));
    let auth = AuthScheme::RpoFalcon512 { pub_key: secret_key.public_key().into() };
    let init_seed: [u8; 32] = rng.random();

    let wallet_account = create_basic_wallet(
        init_seed,
        auth,
        AccountType::RegularAccountImmutableCode,
        AccountStorageMode::Public,
    )?;

    Ok((wallet_account, secret_key))
}
