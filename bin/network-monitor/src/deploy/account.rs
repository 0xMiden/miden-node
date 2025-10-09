//! Core account deployment functionality.

use std::path::Path;

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

/// Ensure accounts exist, creating them if they don't.
///
/// This function checks if the wallet and counter account files exist.
/// If they don't exist, it creates new accounts and saves them to the specified files.
/// If they do exist, it does nothing.
///
/// # Arguments
///
/// * `wallet_file` - Path to the wallet account file.
/// * `counter_file` - Path to the counter program account file.
///
/// # Returns
///
/// `Ok(())` if the accounts exist or were successfully created, or an error if creation fails.
pub fn ensure_accounts_exist(wallet_file: &str, counter_file: &str) -> Result<()> {
    let wallet_exists = Path::new(wallet_file).exists();
    let counter_exists = Path::new(counter_file).exists();

    if wallet_exists && counter_exists {
        tracing::info!("Account files already exist, skipping account creation");
        return Ok(());
    }

    tracing::info!("Account files not found, creating new accounts");

    // Create wallet account
    let (wallet_account, secret_key) = crate::deploy::wallet::create_wallet_account()?;

    // Create counter program account
    let counter_account = crate::deploy::counter::create_counter_account(&wallet_account)?;

    // Save accounts to files
    save_wallet_account(&wallet_account, &secret_key, wallet_file)?;
    save_counter_account(&counter_account, counter_file)?;

    tracing::info!("Successfully created and saved account files");
    Ok(())
}
