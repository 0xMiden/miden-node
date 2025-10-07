//! Deploy account command implementation.
//!
//! This module contains the implementation for deploying Miden accounts to the network.

use anyhow::Result;

use crate::cli::DeployAccountConfig;
use crate::deploy::counter::create_counter_account;
use crate::deploy::wallet::create_wallet_account;
use crate::deploy::{save_counter_account, save_wallet_account};

/// Deploy accounts to the network.
///
/// This function creates both a wallet account and a counter program account,
/// then saves them to the specified files.
pub async fn deploy_accounts(config: DeployAccountConfig) -> Result<()> {
    // Create wallet account
    let (wallet_account, secret_key) = create_wallet_account()?;

    // Create counter program account
    let counter_account = create_counter_account(&wallet_account)?;

    // Save accounts to files
    save_wallet_account(&wallet_account, &secret_key, &config.wallet_file)?;
    save_counter_account(&counter_account, &config.counter_file)?;

    Ok(())
}
