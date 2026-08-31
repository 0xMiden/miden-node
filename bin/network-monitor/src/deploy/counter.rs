//! Counter program account creation functionality.

use std::collections::BTreeSet;

use anyhow::Result;
use miden_node_tracing::miden_instrument;
use miden_protocol::account::component::AccountComponentMetadata;
use miden_protocol::account::{
    Account,
    AccountBuilder,
    AccountComponent,
    AccountId,
    AccountType,
    StorageSlot,
    StorageSlotName,
};
use miden_protocol::asset::AssetAmount;
use miden_protocol::utils::sync::LazyLock;
use miden_protocol::{Felt, Word};
use miden_standards::account::auth::AuthNetworkAccount;
use miden_standards::account::fees::{BasicConstantFeePolicy, FeePolicyManager};
use miden_standards::code_builder::CodeBuilder;
use miden_standards::tx_script::ExpirationTransactionScript;

use crate::COMPONENT;
use crate::counter::create_increment_script;

pub static OWNER_SLOT_NAME: LazyLock<StorageSlotName> = LazyLock::new(|| {
    StorageSlotName::new("miden::monitor::counter_contract::owner")
        .expect("storage slot name should be valid")
});

pub static COUNTER_SLOT_NAME: LazyLock<StorageSlotName> = LazyLock::new(|| {
    StorageSlotName::new("miden::monitor::counter_contract::counter")
        .expect("storage slot name should be valid")
});

/// Create a counter program account with custom MASM script.
#[miden_instrument(
    target = COMPONENT,
    name = "create-counter-account",
    ret(level = "debug"),
)]
pub fn create_counter_account(
    owner_account_id: AccountId,
    fee_faucet_id: AccountId,
) -> Result<Account> {
    // Load and customize the MASM script
    let script =
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/assets/counter_program.masm"));

    // Compile the account code
    let owner_account_id_prefix = owner_account_id.prefix().as_felt();
    let owner_account_id_suffix = owner_account_id.suffix();

    let owner_id_slot = StorageSlot::with_value(
        OWNER_SLOT_NAME.clone(),
        Word::from([owner_account_id_suffix, owner_account_id_prefix, Felt::ZERO, Felt::ZERO]),
    );

    let counter_slot = StorageSlot::with_value(COUNTER_SLOT_NAME.clone(), Word::empty());

    let component_code =
        CodeBuilder::default().compile_component_code("counter::program", script)?;

    let metadata = AccountComponentMetadata::new("counter::program");
    let account_code =
        AccountComponent::new(component_code, vec![counter_slot, owner_id_slot], metadata)?;

    let mut allowed_scripts = BTreeSet::new();

    let increment_script = create_increment_script().expect("is valid note script");

    allowed_scripts.insert(increment_script.root());

    // The account's auth procedure prices every note it consumes, and any transaction creating a
    // note targeted at it prices that note through the same policy via FPI. Every allowlisted note
    // script must have a schedule entry, since a script without one aborts fee estimation.
    let fee_policy = BasicConstantFeePolicy::new()
        .with_fees([(increment_script.root(), AssetAmount::ZERO)])
        .into();
    let fee_policy_manager = FeePolicyManager::builder()
        .fee_faucet_id(fee_faucet_id)
        .active_fee_policy(fee_policy)
        .build();

    let auth_component = AuthNetworkAccount::custom(allowed_scripts, fee_policy_manager)?
        .with_allowed_tx_scripts([ExpirationTransactionScript::script_root()]);

    let init_seed: [u8; 32] = rand::random();
    let counter_account = AccountBuilder::new(init_seed)
        .account_type(AccountType::Public)
        .with_components(auth_component)
        .with_component(account_code)
        .build()?;

    Ok(counter_account)
}

// TESTS
// ================================================================================================

#[cfg(test)]
mod tests {
    use miden_protocol::account::StorageMapKey;
    use miden_protocol::asset::{AssetId, FungibleAsset};
    use miden_standards::account::auth::NetworkAccount;
    use miden_standards::note::{FeeSponsorshipNote, NetworkAccountConfigNote};

    use super::*;
    use crate::deploy::wallet::create_wallet_account;

    /// Every note script the account allowlists must also be priced. `NetworkAccount::new` does not
    /// look at fee-policy storage at all, so without this the fee wiring could be deleted whole and
    /// every other test would stay green while the live FPI aborted fee estimation.
    #[test]
    fn every_allowlisted_note_script_is_priced_at_zero() {
        let (wallet, _secret_key) = create_wallet_account().expect("wallet account should build");
        let fee_faucet_id = FungibleAsset::mock_issuer();
        let counter = create_counter_account(wallet.id(), fee_faucet_id)
            .expect("counter account should build");

        let allowlisted = NetworkAccount::new(counter.clone())
            .expect("counter should be a valid network account")
            .allowed_notes()
            .allowed_script_roots()
            .clone();
        assert_eq!(
            allowlisted.len(),
            1,
            "only the increment note may be allowlisted, got {allowlisted:?}"
        );

        // A scheduled entry is `[fee_amount, 0, 0, 1]`: the trailing set-marker is what
        // distinguishes an explicit zero fee from an absent key, since storage maps prune zero
        // words and return the zero word for anything unset.
        let expected_entry = Word::from([Felt::ZERO, Felt::ZERO, Felt::ZERO, Felt::ONE]);
        for root in &allowlisted {
            let entry = counter
                .storage()
                .get_map_item(
                    BasicConstantFeePolicy::fee_schedule_slot_name(),
                    StorageMapKey::new(root.as_word()),
                )
                .expect("the fee schedule slot should be a map");
            assert_eq!(
                entry, expected_entry,
                "note script root {root} is allowlisted but has no zero-fee schedule entry"
            );
        }
    }

    /// The counter carries no `Authority` component, so the two notes `AuthNetworkAccount::new`
    /// would allowlist by default must stay out of the allowlist: a `NETWORK_ACCOUNT_CONFIG` note
    /// anyone could send would abort in `assert_authorized`, and an unpaired `FEE_SPONSORSHIP` note
    /// would abort fee collection. Both aborts are network transactions failing against a public
    /// account, which is exactly what the tracking card reports as unhealthy.
    #[test]
    fn counter_does_not_allowlist_notes_it_cannot_service() {
        let (wallet, _secret_key) = create_wallet_account().expect("wallet account should build");
        let counter = create_counter_account(wallet.id(), FungibleAsset::mock_issuer())
            .expect("counter account should build");

        let network_account =
            NetworkAccount::new(counter).expect("counter should be a valid network account");
        let allowlisted = network_account.allowed_notes().allowed_script_roots();

        assert!(
            !allowlisted.contains(&NetworkAccountConfigNote::script_root()),
            "the config note needs an Authority component the counter does not have"
        );
        assert!(
            !allowlisted.contains(&FeeSponsorshipNote::script_root()),
            "the counter prices its notes at zero, so it never collects sponsored fees"
        );
        // Dropping the defaults must not cost the account its serviceability: the ntx builder
        // attaches the expiration script to every network transaction, and the store classifies an
        // account whose tx-script allowlist lacks that root as non-network.
        assert!(
            network_account.allows_tx_script(&ExpirationTransactionScript::script_root()),
            "the canonical expiration tx script must stay allowlisted"
        );
    }

    /// The fee policy must be the *active* one and denominated in the faucet passed in, otherwise
    /// fee estimation dispatches nowhere or charges the wrong asset.
    #[test]
    fn fee_policy_is_active_and_uses_the_given_faucet() {
        let (wallet, _secret_key) = create_wallet_account().expect("wallet account should build");
        let fee_faucet_id = FungibleAsset::mock_issuer();
        let counter = create_counter_account(wallet.id(), fee_faucet_id)
            .expect("counter account should build");

        let active = counter
            .storage()
            .get_item(FeePolicyManager::active_fee_policy_slot())
            .expect("the active fee policy slot should exist");
        assert_eq!(
            active,
            BasicConstantFeePolicy::root().as_word(),
            "the basic constant fee policy must be the active policy"
        );

        let fee_asset = counter
            .storage()
            .get_item(FeePolicyManager::fee_asset_id_slot())
            .expect("the fee asset slot should exist");
        assert_eq!(
            fee_asset,
            AssetId::new_fungible(fee_faucet_id).to_word(),
            "fees must be charged in the fee faucet's asset"
        );
    }
}
