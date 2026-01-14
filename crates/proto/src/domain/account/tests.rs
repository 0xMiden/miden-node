use miden_protocol::crypto::merkle::EmptySubtreeRoots;
use miden_protocol::crypto::merkle::smt::SMT_DEPTH;

use super::*;

fn word_from_u32(arr: [u32; 4]) -> Word {
    Word::from(arr)
}

fn test_slot_name() -> StorageSlotName {
    StorageSlotName::new("miden::test::storage::slot").unwrap()
}

fn empty_smt_root() -> Word {
    *EmptySubtreeRoots::entry(SMT_DEPTH, 0)
}

#[test]
fn account_storage_map_details_from_forest_entries() {
    let slot_name = test_slot_name();
    let entries = vec![
        (word_from_u32([1, 2, 3, 4]), word_from_u32([5, 6, 7, 8])),
        (word_from_u32([9, 10, 11, 12]), word_from_u32([13, 14, 15, 16])),
    ];

    let details = AccountStorageMapDetails::from_forest_entries(slot_name.clone(), entries.clone());

    assert_eq!(details.slot_name, slot_name);
    assert_eq!(details.entries, StorageMapEntries::AllEntries(entries));
}

#[test]
fn account_storage_map_details_from_forest_entries_limit_exceeded() {
    let slot_name = test_slot_name();
    // Create more entries than MAX_RETURN_ENTRIES
    let entries: Vec<_> = (0..=AccountStorageMapDetails::MAX_RETURN_ENTRIES)
        .map(|i| {
            let key = word_from_u32([i as u32, 0, 0, 0]);
            let value = word_from_u32([0, 0, 0, i as u32]);
            (key, value)
        })
        .collect();

    let details = AccountStorageMapDetails::from_forest_entries(slot_name.clone(), entries);

    assert_eq!(details.slot_name, slot_name);
    assert_eq!(details.entries, StorageMapEntries::LimitExceeded);
}

#[test]
fn account_storage_map_details_from_specific_keys() {
    let slot_name = test_slot_name();

    // Create an SmtForest and populate it with some data
    let mut forest = SmtForest::new();
    let entries = [
        (word_from_u32([1, 0, 0, 0]), word_from_u32([10, 0, 0, 0])),
        (word_from_u32([2, 0, 0, 0]), word_from_u32([20, 0, 0, 0])),
        (word_from_u32([3, 0, 0, 0]), word_from_u32([30, 0, 0, 0])),
    ];

    // Insert entries into the forest starting from an empty root
    let smt_root = forest.batch_insert(empty_smt_root(), entries.iter().copied()).unwrap();

    // Query specific keys
    let keys = vec![word_from_u32([1, 0, 0, 0]), word_from_u32([3, 0, 0, 0])];

    let details =
        AccountStorageMapDetails::from_specific_keys(slot_name.clone(), &keys, &forest, smt_root)
            .unwrap();

    assert_eq!(details.slot_name, slot_name);
    match details.entries {
        StorageMapEntries::EntriesWithProofs(proofs) => {
            use miden_protocol::crypto::merkle::smt::SmtLeaf;

            assert_eq!(proofs.len(), 2);

            // Helper to extract key-value from any leaf type
            let get_value = |proof: &SmtProof, expected_key: Word| -> Word {
                match proof.leaf() {
                    SmtLeaf::Single((k, v)) if *k == expected_key => *v,
                    SmtLeaf::Multiple(entries) => entries
                        .iter()
                        .find(|(k, _)| *k == expected_key)
                        .map_or(miden_protocol::EMPTY_WORD, |(_, v)| *v),
                    _ => miden_protocol::EMPTY_WORD,
                }
            };

            let first_key = word_from_u32([1, 0, 0, 0]);
            let second_key = word_from_u32([3, 0, 0, 0]);
            let first_value = get_value(&proofs[0], first_key);
            let second_value = get_value(&proofs[1], second_key);

            assert_eq!(first_value, word_from_u32([10, 0, 0, 0]));
            assert_eq!(second_value, word_from_u32([30, 0, 0, 0]));
        },
        StorageMapEntries::LimitExceeded | StorageMapEntries::AllEntries(_) => {
            panic!("Expected EntriesWithProofs")
        },
    }
}

#[test]
fn account_storage_map_details_from_specific_keys_nonexistent_returns_proof() {
    let slot_name = test_slot_name();

    // Create an SmtForest with one entry so the root is tracked
    let mut forest = SmtForest::new();
    let entries = [(word_from_u32([1, 0, 0, 0]), word_from_u32([10, 0, 0, 0]))];
    let smt_root = forest.batch_insert(empty_smt_root(), entries.iter().copied()).unwrap();

    // Query a key that doesn't exist in the tree - should return a proof
    // (the proof will show non-membership or point to an adjacent leaf)
    let keys = vec![word_from_u32([99, 0, 0, 0])];

    let details =
        AccountStorageMapDetails::from_specific_keys(slot_name.clone(), &keys, &forest, smt_root)
            .unwrap();

    match details.entries {
        StorageMapEntries::EntriesWithProofs(proofs) => {
            // We got a proof for the non-existent key
            assert_eq!(proofs.len(), 1);
            // The proof exists and can be used to verify non-membership
        },
        StorageMapEntries::LimitExceeded | StorageMapEntries::AllEntries(_) => {
            panic!("Expected EntriesWithProofs")
        },
    }
}

#[test]
fn account_storage_map_details_from_specific_keys_limit_exceeded() {
    let slot_name = test_slot_name();
    let mut forest = SmtForest::new();

    // Create a forest with some data to get a valid root
    let entries = [(word_from_u32([1, 0, 0, 0]), word_from_u32([10, 0, 0, 0]))];
    let smt_root = forest.batch_insert(empty_smt_root(), entries.iter().copied()).unwrap();

    // Create more keys than MAX_RETURN_ENTRIES
    let keys: Vec<_> = (0..=AccountStorageMapDetails::MAX_RETURN_ENTRIES)
        .map(|i| word_from_u32([i as u32, 0, 0, 0]))
        .collect();

    let details =
        AccountStorageMapDetails::from_specific_keys(slot_name.clone(), &keys, &forest, smt_root)
            .unwrap();

    assert_eq!(details.entries, StorageMapEntries::LimitExceeded);
}
