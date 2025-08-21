// @generated automatically by Diesel CLI.
diesel::table! {
    account_storage_map_values (account_id, block_num, slot, key, is_latest_update) {
        account_id -> Binary,
        block_num -> BigInt,
        slot -> Integer,
        key -> Binary,
        value -> Binary,
        is_latest_update -> Bool,
    }
}
diesel::table! {
    accounts (account_id) {
        account_id -> Binary,
        network_account_id_prefix -> Nullable<BigInt>,
        account_commitment -> Binary,
        code_commitment -> Nullable<Binary>,
        storage -> Nullable<Binary>,
        vault -> Nullable<Binary>,
        nonce -> Nullable<BigInt>,
        block_num -> BigInt,
    }
}

diesel::table! {
    account_codes (code_commitment) {
        code_commitment -> Binary,
        code -> Binary,
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
    notes (committed_at, batch_index, note_index) {
        committed_at -> BigInt,
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
        consumed_at -> Nullable<BigInt>,
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

diesel::joinable!(accounts -> account_codes (code_commitment));
diesel::joinable!(accounts -> block_headers (block_num));
diesel::joinable!(notes -> accounts (sender));
diesel::joinable!(notes -> block_headers (committed_at));
diesel::joinable!(notes -> note_scripts (script_root));
diesel::joinable!(nullifiers -> block_headers (block_num));
diesel::joinable!(transactions -> accounts (account_id));
diesel::joinable!(transactions -> block_headers (block_num));

diesel::allow_tables_to_appear_in_same_query!(
    account_codes,
    account_storage_map_values,
    accounts,
    block_headers,
    note_scripts,
    notes,
    nullifiers,
    transactions,
);
