// @generated automatically by Diesel CLI.

diesel::table! {
    account_deltas (account_id, block_num) {
        account_id -> Binary,
        block_num -> BigInt,
        nonce -> BigInt,
    }
}

diesel::table! {
    account_fungible_asset_deltas (account_id, block_num, faucet_id) {
        account_id -> Binary,
        block_num -> BigInt,
        faucet_id -> Binary,
        delta -> BigInt,
    }
}

diesel::table! {
    account_non_fungible_asset_updates (account_id, block_num, vault_key) {
        account_id -> Binary,
        block_num -> BigInt,
        vault_key -> Binary,
        is_remove -> Integer, // TODO consider migration to Boolean?
    }
}

diesel::table! {
    account_storage_map_updates (account_id, block_num, slot, key) {
        account_id -> Binary,
        block_num -> BigInt,
        slot -> Integer,
        key -> Binary,
        value -> Binary,
    }
}

diesel::table! {
    account_storage_slot_updates (account_id, block_num, slot) {
        account_id -> Binary,
        block_num -> BigInt,
        slot -> Integer,
        value -> Binary,
    }
}

diesel::table! {
    accounts (account_id) {
        account_id -> Binary,
        network_account_id_prefix -> Nullable<BigInt>,
        account_commitment -> Binary,
        block_num -> BigInt,
        details -> Nullable<Binary>,
    }
}

diesel::table! {
    block_headers (block_num) {
        block_num -> BigInt,
        block_header -> Binary,
    }
}

diesel::table! {
    note_scripts (script_root) {
        script_root -> Binary,
        script -> Binary,
    }
}

diesel::table! {
    notes (block_num, batch_index, note_index) {
        block_num -> BigInt,
        batch_index -> Integer,
        note_index -> Integer,
        note_id -> Binary,
        note_type -> Integer,
        sender -> Binary,
        tag -> Integer,
        execution_mode -> Integer,
        aux -> BigInt,
        execution_hint -> BigInt,
        inclusion_path -> Binary,
        consumed -> Integer,
        nullifier -> Nullable<Binary>,
        assets -> Nullable<Binary>,
        inputs -> Nullable<Binary>,
        script_root -> Nullable<Binary>,
        serial_num -> Nullable<Binary>,
    }
}

diesel::table! {
    nullifiers (nullifier) {
        nullifier -> Binary,
        nullifier_prefix -> Integer,
        block_num -> BigInt,
    }
}

diesel::table! {
    transactions (transaction_id) {
        transaction_id -> Binary,
        account_id -> Binary,
        block_num -> BigInt,
    }
}

diesel::joinable!(account_deltas -> accounts (account_id));
diesel::joinable!(account_deltas -> block_headers (block_num));
diesel::joinable!(accounts -> block_headers (block_num));
diesel::joinable!(notes -> accounts (sender));
diesel::joinable!(notes -> block_headers (block_num));
diesel::joinable!(notes -> note_scripts (script_root));
diesel::joinable!(nullifiers -> block_headers (block_num));
diesel::joinable!(transactions -> accounts (account_id));
diesel::joinable!(transactions -> block_headers (block_num));

diesel::allow_tables_to_appear_in_same_query!(
    account_deltas,
    account_fungible_asset_deltas,
    account_non_fungible_asset_updates,
    account_storage_map_updates,
    account_storage_slot_updates,
    accounts,
    block_headers,
    note_scripts,
    notes,
    nullifiers,
    settings,
    transactions,
);
