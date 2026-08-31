//! Counter program account creation functionality.

use std::collections::BTreeSet;

use anyhow::Result;
use miden_node_utils::tracing::miden_instrument;
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
use miden_standards::account::wallets::BasicWallet;
use miden_standards::code_builder::CodeBuilder;
use miden_standards::note::{FeeSponsorshipNote, P2idNote};
use miden_standards::tx_script::ExpirationTransactionScript;

use crate::COMPONENT;
use crate::counter::create_increment_script;
use crate::funding::max_fee_per_transaction;

pub static OWNER_SLOT_NAME: LazyLock<StorageSlotName> = LazyLock::new(|| {
    StorageSlotName::new("miden::monitor::counter_contract::owner")
        .expect("storage slot name should be valid")
});

pub static COUNTER_SLOT_NAME: LazyLock<StorageSlotName> = LazyLock::new(|| {
    StorageSlotName::new("miden::monitor::counter_contract::counter")
        .expect("storage slot name should be valid")
});

/// Create a counter program account with custom MASM script.
///
/// On a fee-charging chain (`verification_base_fee > 0`) the account additionally:
/// - prices the increment note at the per-transaction fee bound, so each increment attaches a
///   `FEE_SPONSORSHIP` note paying for the network transaction that consumes it;
/// - allowlists (at zero price) the `FEE_SPONSORSHIP` note and the P2ID note funding its
///   creation fee;
/// - carries `BasicWallet`, since the P2ID script claims assets via `receive_asset`.
///
/// On a zero-fee chain the account keeps its minimal shape: only the increment note is
/// allowlisted, priced at zero.
#[miden_instrument(
    target = COMPONENT,
    name = "create-counter-account",
    ret(level = "debug"),
)]
pub fn create_counter_account(
    owner_account_id: AccountId,
    fee_faucet_id: AccountId,
    verification_base_fee: u32,
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
    let increment_note_fee = AssetAmount::new(max_fee_per_transaction(verification_base_fee))
        .expect("the per-transaction fee bound fits an asset amount");
    let mut fee_schedule = vec![(increment_script.root(), increment_note_fee)];

    if verification_base_fee > 0 {
        allowed_scripts.insert(FeeSponsorshipNote::script_root());
        fee_schedule.push((FeeSponsorshipNote::script_root(), AssetAmount::ZERO));
        allowed_scripts.insert(P2idNote::script_root());
        fee_schedule.push((P2idNote::script_root(), AssetAmount::ZERO));
    }

    let fee_policy = BasicConstantFeePolicy::new().with_fees(fee_schedule).into();
    let fee_policy_manager = FeePolicyManager::builder()
        .fee_faucet_id(fee_faucet_id)
        .active_fee_policy(fee_policy)
        .build();

    let auth_component = AuthNetworkAccount::custom(allowed_scripts, fee_policy_manager)?
        .with_allowed_tx_scripts([ExpirationTransactionScript::script_root()]);

    let init_seed: [u8; 32] = rand::random();
    let mut builder = AccountBuilder::new(init_seed)
        .account_type(AccountType::Public)
        .with_components(auth_component)
        .with_component(account_code);
    if verification_base_fee > 0 {
        builder = builder.with_component(BasicWallet);
    }
    let counter_account = builder.build()?;

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
        let counter = create_counter_account(wallet.id(), fee_faucet_id, 0)
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
        let counter = create_counter_account(wallet.id(), FungibleAsset::mock_issuer(), 0)
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
        let counter = create_counter_account(wallet.id(), fee_faucet_id, 0)
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

    /// On a fee-charging chain the counter must collect sponsorships, consume the faucet's P2ID
    /// note, and price the increment note so senders sponsor its network transactions.
    #[test]
    fn fee_charging_counter_allowlists_and_prices_its_funding_notes() {
        const BASE_FEE: u32 = 500;
        let (wallet, _secret_key) = create_wallet_account().expect("wallet account should build");
        let fee_faucet_id = FungibleAsset::mock_issuer();
        let counter = create_counter_account(wallet.id(), fee_faucet_id, BASE_FEE)
            .expect("counter account should build");

        let network_account = NetworkAccount::new(counter.clone())
            .expect("counter should be a valid network account");
        let allowlisted = network_account.allowed_notes().allowed_script_roots();

        assert!(
            allowlisted.contains(&FeeSponsorshipNote::script_root()),
            "sponsorship notes are the counter's only fee income and must be consumable"
        );
        assert!(
            allowlisted.contains(&P2idNote::script_root()),
            "the faucet's P2ID note funds the creation fee and must be consumable"
        );
        assert!(
            !allowlisted.contains(&NetworkAccountConfigNote::script_root()),
            "the config note still needs an Authority component the counter does not have"
        );

        let schedule_entry = |root: miden_protocol::note::NoteScriptRoot| {
            counter
                .storage()
                .get_map_item(
                    BasicConstantFeePolicy::fee_schedule_slot_name(),
                    StorageMapKey::new(root.as_word()),
                )
                .expect("the fee schedule slot should be a map")
        };

        let increment_root = create_increment_script().expect("is valid note script").root();
        let expected_price =
            Felt::new(max_fee_per_transaction(BASE_FEE)).expect("price fits the field");
        assert_eq!(
            schedule_entry(increment_root),
            Word::from([expected_price, Felt::ZERO, Felt::ZERO, Felt::ONE]),
            "the increment note must be priced at the per-transaction fee bound"
        );

        let zero_entry = Word::from([Felt::ZERO, Felt::ZERO, Felt::ZERO, Felt::ONE]);
        assert_eq!(schedule_entry(FeeSponsorshipNote::script_root()), zero_entry);
        assert_eq!(schedule_entry(P2idNote::script_root()), zero_entry);
    }
}
