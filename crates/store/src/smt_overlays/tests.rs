use miden_objects::account::{AccountIdVersion, AccountStorageMode, AccountType};
use miden_objects::testing::account_id::{
    ACCOUNT_ID_PRIVATE_SENDER,
    ACCOUNT_ID_REGULAR_PRIVATE_ACCOUNT_UPDATABLE_CODE,
    ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_IMMUTABLE_CODE,
};

use super::*;

fn acc(seed: u8) -> AccountId {
    create_test_account_id(seed as _)
}

fn word(val: u8) -> Word {
    Word::from([Felt::new(val as u64); 4])
}

fn test_account_tree() -> AccountTree {
    let mut atree = AccountTree::new();
    let commitments = vec![(acc(11), word(0x11)), (acc(22), word(0x22)), (acc(33), word(0x33))];
    let mutations = atree.compute_mutations(commitments).unwrap();
    atree.apply_mutations(mutations).unwrap();
    atree
}

fn create_test_account_id(seed: u64) -> AccountId {
    // Use predefined test account IDs
    let test_ids = [
        ACCOUNT_ID_PRIVATE_SENDER,
        ACCOUNT_ID_REGULAR_PRIVATE_ACCOUNT_UPDATABLE_CODE,
        ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_IMMUTABLE_CODE,
    ];
    AccountId::try_from(test_ids[(seed % 3) as usize]).unwrap()
}

fn create_test_overlay(num_mutations: usize) -> Overlay {
    let mut mutated = HashMap::new();
    for i in 0..num_mutations {
        let key = Word::from([Felt::new(i as u64), ZERO, ZERO, ZERO]);
        // Create a simple node index for testing
        let idx = NodeIndex::new(SMT_DEPTH, i as u64).unwrap();
        let value = Word::from([Felt::new((i * 10) as u64), ZERO, ZERO, ZERO]);
        let leaf = SmtLeaf::new_single(key, value);
        mutated.insert(idx, leaf);
    }

    Overlay {
        old_root: Word::default(),
        new_root: Word::from([Felt::new(1), ZERO, ZERO, ZERO]),
        mutated,
    }
}

#[test]
fn test_overlay_with_0_overlays() {
    let smt = Smt::default();
    let piped_trie = SmtWithOverlays::new(smt, BlockNumber::from(0u32));

    // Test with 0 overlays
    assert_eq!(piped_trie.overlays.len(), 0);
    assert_eq!(piped_trie.oldest(), BlockNumber::from(0u32));

    // Test at_height with no overlays
    assert_eq!(piped_trie.at_height(BlockNumber::from(0u32)), Some(piped_trie.latest.root()));
    assert_eq!(piped_trie.at_height(BlockNumber::from(1u32)), None);
}

#[test]
fn test_overlay_with_1_overlay() {
    let smt = Smt::default();
    let mut piped_trie = SmtWithOverlays::new(smt, BlockNumber::from(0u32));

    // Add 1 overlay
    let overlay = create_test_overlay(3);
    piped_trie.add_overlay(overlay.clone());

    assert_eq!(piped_trie.overlays.len(), 1);
    assert_eq!(piped_trie.oldest(), BlockNumber::from(0u32));

    // Test at_height with 1 overlay
    assert_eq!(piped_trie.at_height(BlockNumber::from(0u32)), Some(piped_trie.latest.root()));
    assert_eq!(piped_trie.at_height(BlockNumber::from(1u32)), Some(overlay.root()));
    assert_eq!(piped_trie.at_height(BlockNumber::from(2u32)), None);
}

#[test]
fn test_overlay_with_5_overlays() {
    let smt = Smt::default();
    let mut piped_trie = SmtWithOverlays::new(smt, BlockNumber::from(0u32));

    // Add 5 overlays with different numbers of mutations
    let overlays: Vec<Overlay> = (1..=5).map(|i| create_test_overlay(i * 2)).collect();

    for overlay in overlays.iter() {
        piped_trie.add_overlay(overlay.clone());
    }

    assert_eq!(piped_trie.overlays.len(), 5);
    assert_eq!(piped_trie.oldest(), BlockNumber::from(0u32));

    // Test at_height with 5 overlays
    assert_eq!(piped_trie.at_height(BlockNumber::from(0u32)), Some(piped_trie.latest.root()));

    // Check that overlays are in the correct order (most recent first)
    for i in 0..5 {
        // overlays were added in order 0,1,2,3,4 but stored in reverse
        let expected_root = overlays[4 - i].root();
        assert_eq!(piped_trie.at_height(BlockNumber::from((i + 1) as u32)), Some(expected_root));
    }

    assert_eq!(piped_trie.at_height(BlockNumber::from(6u32)), None);
}

#[test]
fn test_overlay_with_non_empty_overlays() {
    let smt = Smt::default();
    let mut piped_trie = SmtWithOverlays::new(smt, BlockNumber::from(10u32)); // Start at block 10

    // Create overlays with actual content
    let mut overlay1_mutated = HashMap::new();
    let key1 = Word::from([Felt::new(1), ZERO, ZERO, ZERO]);
    let idx1 = NodeIndex::new(SMT_DEPTH, 1u64).unwrap();
    overlay1_mutated.insert(idx1, SmtLeaf::new_single(key1, key1));

    let overlay1 = Overlay {
        old_root: Word::default(),
        new_root: Word::from([Felt::new(100), ZERO, ZERO, ZERO]),
        mutated: overlay1_mutated,
    };

    let mut overlay2_mutated = HashMap::new();
    let key2 = Word::from([Felt::new(2), ZERO, ZERO, ZERO]);
    let idx2 = NodeIndex::new(SMT_DEPTH, 2u64).unwrap();
    overlay2_mutated.insert(idx2, SmtLeaf::new_single(key2, key2));

    let overlay2 = Overlay {
        old_root: Word::from([Felt::new(100), ZERO, ZERO, ZERO]),
        new_root: Word::from([Felt::new(200), ZERO, ZERO, ZERO]),
        mutated: overlay2_mutated,
    };

    piped_trie.add_overlay(overlay1.clone());
    piped_trie.add_overlay(overlay2.clone());

    assert_eq!(piped_trie.overlays.len(), 2);
    assert_eq!(piped_trie.oldest(), BlockNumber::from(10u32));

    // Test at_height
    assert_eq!(piped_trie.at_height(BlockNumber::from(10u32)), Some(piped_trie.latest.root()));
    assert_eq!(piped_trie.at_height(BlockNumber::from(11u32)), Some(overlay1.root()));
    assert_eq!(piped_trie.at_height(BlockNumber::from(12u32)), Some(overlay2.root()));
    assert_eq!(piped_trie.at_height(BlockNumber::from(13u32)), None);
}

#[test]
fn test_stacked_trie_root() {
    let account_tree = AccountTree::new();

    // Test with no overlays
    let stacked_trie = HistoricalTreeView {
        block_number: BlockNumber::from(0u32),
        latest: &account_tree,
        stacks: &[],
    };
    assert_eq!(stacked_trie.root(), account_tree.root());

    // Test with overlays
    let overlay = create_test_overlay(2);
    let overlays = vec![overlay.clone()];

    let stacked_trie_with_overlay = HistoricalTreeView {
        block_number: BlockNumber::from(1u32),
        latest: &account_tree,
        stacks: &overlays,
    };
    assert_eq!(stacked_trie_with_overlay.root(), overlay.root());
}

#[test]
fn test_stacked_trie_get_value() {
    let account_tree = AccountTree::new();
    let account_id = create_test_account_id(42);

    // Create overlay with specific account
    let key = id_to_smt_key(account_id);
    let idx = NodeIndex::new(SMT_DEPTH, key[3].as_int()).unwrap();
    let mut mutated = HashMap::new();
    let test_value = Word::from([Felt::new(999), ZERO, ZERO, ZERO]);
    mutated.insert(idx, SmtLeaf::new_single(key, test_value));

    let overlay = Overlay {
        old_root: Word::default(),
        new_root: Word::from([Felt::new(1), ZERO, ZERO, ZERO]),
        mutated,
    };

    let overlays = vec![overlay];
    let stacked_trie = HistoricalTreeView {
        block_number: BlockNumber::from(0u32),
        latest: &account_tree,
        stacks: &overlays,
    };

    // Should return the value from the overlay
    let value = stacked_trie.get_value(&account_id);
    assert_eq!(value, test_value);
}

#[test]
fn test_piped_trie_cleanup() {
    let smt = Smt::default();
    let mut piped_trie = SmtWithOverlays::new(smt, BlockNumber::from(0u32));

    // Add more than MAX_OVERLAYS (10) overlays
    for i in 0..15 {
        let overlay = create_test_overlay(i + 1);
        piped_trie.add_overlay(overlay);
    }

    // Should only keep MAX_OVERLAYS
    assert_eq!(piped_trie.overlays.len(), 10);
}

#[test]
fn test_id_to_smt_key() {
    let account_id = create_test_account_id(123);
    let key = id_to_smt_key(account_id);

    // Check that the key is properly formatted
    assert_eq!(key[2], account_id.suffix());
    assert_eq!(key[3], account_id.prefix().as_felt());
    assert_eq!(key[0], ZERO);
    assert_eq!(key[1], ZERO);
}

#[test]
fn test_open_comparison() {
    // Test that opening with 1 overlay produces same result as modifying AccountTree directly
    let mut account_tree_direct = test_account_tree();
    let account_id = create_test_account_id(42);
    let test_value = Word::from([Felt::new(999), ZERO, ZERO, ZERO]);

    // Modify AccountTree directly
    let commitments = vec![(account_id, test_value)];
    let mutations = account_tree_direct.compute_mutations(commitments.clone()).unwrap();
    account_tree_direct.apply_mutations(mutations.clone()).unwrap();

    // Create StackedTrie with overlay
    let account_tree_base = test_account_tree();
    let key = id_to_smt_key(account_id);
    let idx = NodeIndex::new(SMT_DEPTH, key[3].as_int()).unwrap();
    let mut mutated = HashMap::new();
    mutated.insert(idx, SmtLeaf::new_single(key, test_value));

    let overlay = Overlay {
        old_root: account_tree_base.root(),
        new_root: account_tree_direct.root(),
        mutated,
    };

    let stacked_trie = HistoricalTreeView {
        block_number: BlockNumber::from(1u32),
        latest: &account_tree_base,
        stacks: &[overlay],
    };

    assert_eq!(stacked_trie.open(&key), account_tree_direct.open(account_id).into_proof());
}
