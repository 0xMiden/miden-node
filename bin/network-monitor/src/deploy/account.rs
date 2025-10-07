//! Core account deployment functionality.

use anyhow::Result;
use miden_objects::account::{Account, AccountFile, AuthSecretKey};
use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;

/// Save wallet account to file with authentication keys.
pub fn save_wallet_account(
    account: &Account,
    secret_key: &SecretKey,
    file_path: &str,
) -> Result<()> {
    let auth_secret_key = AuthSecretKey::RpoFalcon512(secret_key.clone());
    let account_file = AccountFile::new(account.clone(), vec![auth_secret_key]);
    account_file.write(file_path)?;
    Ok(())
}

/// Save counter program account to file.
pub fn save_counter_account(account: &Account, file_path: &str) -> Result<()> {
    let account_file = AccountFile::new(account.clone(), vec![]);
    account_file.write(file_path)?;
    Ok(())
}
