//! Counter program account creation functionality.

use std::path::Path;
use std::sync::Arc;

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
    StorageSlotName,
};
use miden_objects::assembly::{DefaultSourceManager, LibraryPath, Module, ModuleKind};
use miden_objects::utils::sync::LazyLock;
use miden_objects::{Felt, FieldElement, Word};
use tracing::instrument;

use crate::COMPONENT;

static OWNER_SLOT_NAME: LazyLock<StorageSlotName> = LazyLock::new(|| {
    StorageSlotName::new("miden::monitor::counter_contract::owner")
        .expect("storage slot name should be valid")
});

static COUNTER_SLOT_NAME: LazyLock<StorageSlotName> = LazyLock::new(|| {
    StorageSlotName::new("miden::monitor::counter_contract::counter")
        .expect("storage slot name should be valid")
});

/// Create a counter program account with custom MASM script.
#[instrument(target = COMPONENT, name = "create-counter-account", skip_all, ret(level = "debug"))]
pub fn create_counter_account(owner_account_id: AccountId) -> Result<Account> {
    // Load and customize the MASM script
    let script =
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/assets/counter_program.masm"));

    // Compile the account code
    let owner_account_id_prefix = owner_account_id.prefix().as_felt();
    let owner_account_id_suffix = owner_account_id.suffix();

    let owner_id_slot = StorageSlot::with_value(
        OWNER_SLOT_NAME.clone(),
        Word::from([Felt::ZERO, Felt::ZERO, owner_account_id_suffix, owner_account_id_prefix]),
    );

    let counter_slot = StorageSlot::with_value(COUNTER_SLOT_NAME.clone(), Word::empty());

    let assembler = TransactionKernel::assembler();
    let source_manager = Arc::new(DefaultSourceManager::default());
    let library_path = LibraryPath::new("counter::program")?;
    let module = Module::parser(ModuleKind::Library)
        .parse_str(library_path, script, &source_manager)
        .map_err(|e| anyhow::anyhow!("Failed to parse module: {e}"))?;
    let library = assembler
        .assemble_library([module])
        .map_err(|e| anyhow::anyhow!("Failed to assemble library: {e}"))?;

    let account_code = AccountComponent::new(library, vec![counter_slot, owner_id_slot])?
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
