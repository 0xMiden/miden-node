diesel::table! {
    transactions (transaction_id) {
        transaction_id -> Binary,
        account_id -> Binary,
        initial_state_commitment -> Binary,
        final_state_commitment -> Binary,
        input_notes -> Binary,
        output_notes -> Binary,
        size_in_bytes -> BigInt,
    }
}
