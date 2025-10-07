//! Counter program account creation functionality.

use anyhow::Result;
use miden_lib::account::auth::NoAuth;
use miden_lib::transaction::TransactionKernel;
use miden_objects::account::{
    Account,
    AccountBuilder,
    AccountComponent,
    AccountStorageMode,
    AccountType,
    StorageSlot,
};

/// Create a counter program account with custom MASM script.
///
/// The counter program includes authentication logic that only allows the specified
/// wallet account to increment the counter.
pub fn create_counter_account(wallet_account: &Account) -> Result<Account> {
    // Get the authorized account ID from the wallet account
    let authorized_id = wallet_account.id();
    let authorized_id_prefix = authorized_id.prefix();
    let authorized_id_suffix = authorized_id.suffix();

    // Load and customize the MASM script
    let script =
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/assets/counter_program.masm"));
    let script =
        script.replace("{AUTHORIZED_ACCOUNT_ID_PREFIX}", &authorized_id_prefix.to_string());
    let script =
        script.replace("{AUTHORIZED_ACCOUNT_ID_SUFFIX}", &authorized_id_suffix.to_string());

    // Compile the account code
    let account_code = AccountComponent::compile(
        script,
        TransactionKernel::assembler(),
        vec![StorageSlot::empty_value()],
    )?
    .with_supports_all_types();

    // Create the counter program account
    let init_seed: [u8; 32] = rand::random();
    let counter_program = AccountBuilder::new(init_seed)
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Network)
        .with_component(account_code)
        .with_auth_component(NoAuth)
        .build()?;

    Ok(counter_program)
}
