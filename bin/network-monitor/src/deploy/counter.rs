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
use tracing::instrument;

use crate::COMPONENT;

/// Create a counter program account with custom MASM script.
#[instrument(target = COMPONENT, name = "create-counter-account", skip_all, ret(level = "debug"))]
pub fn create_counter_account() -> Result<Account> {
    // Load and customize the MASM script
    let script =
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/assets/counter_program.masm"));

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
