//! Counter program account creation functionality.

use std::path::Path;

use anyhow::Result;
use miden_lib::testing::account_component::IncrNonceAuthComponent;
use miden_lib::transaction::TransactionKernel;
use miden_objects::account::{
    Account,
    AccountBuilder,
    AccountComponent,
    AccountFile,
    AccountId,
    AccountStorageMode,
    AccountType,
    StorageSlot,
};
use miden_objects::{Felt, Word};
use tracing::instrument;

use crate::COMPONENT;

/// Create a counter program account with custom MASM script.
#[instrument(target = COMPONENT, name = "create-counter-account", skip_all, ret(level = "debug"))]
pub fn create_counter_account(owner_account_id: AccountId) -> Result<Account> {
    // Load and customize the MASM script
    let script =
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/assets/counter_program.masm"));

    // Compile the account code
    let owner_account_id_prefix = owner_account_id.prefix().as_felt();
    let owner_account_id_suffix = owner_account_id.suffix();

    let owner_word =
        Word::from([Felt::new(0), Felt::new(0), owner_account_id_suffix, owner_account_id_prefix]);

    let account_code = AccountComponent::compile(
        script,
        TransactionKernel::assembler(),
        vec![StorageSlot::empty_value(), StorageSlot::Value(owner_word)],
    )?
    .with_supports_all_types();

    let incr_nonce_auth: AccountComponent = IncrNonceAuthComponent.into();

    // Create the counter program account
    let init_seed: [u8; 32] = rand::random();
    let counter_account = AccountBuilder::new(init_seed)
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Network)
        .with_component(account_code)
        .with_auth_component(incr_nonce_auth)
        .build()?;

    Ok(counter_account)
}

/// Save counter program account to disk without extra auth material.
pub fn save_counter_account(account: &Account, file_path: &Path) -> Result<()> {
    let account_file = AccountFile::new(account.clone(), vec![]);
    account_file.write(file_path)?;
    Ok(())
}
