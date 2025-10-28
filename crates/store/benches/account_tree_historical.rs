use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use miden_crypto::merkle::MemoryStorage;
use miden_node_store::{AccountTreeWithHistory, InMemoryAccountTree};
use miden_objects::Word;
use miden_objects::account::AccountId;
use miden_objects::block::BlockNumber;
use miden_objects::block::account_tree::{AccountTree, account_id_to_smt_key};
use miden_objects::crypto::hash::rpo::Rpo256;
use miden_objects::crypto::merkle::LargeSmt;
use miden_objects::testing::account_id::AccountIdBuilder;

// HELPER FUNCTIONS
// ================================================================================================

/// Creates a storage backend for a `LargeSmt`.
fn setup_storage() -> MemoryStorage {
    // TODO migrate to RocksDB for persistence to gain meaningful numbers
    MemoryStorage::default()
}

/// Generates a deterministic word from a seed.
fn generate_word(seed: &mut [u8; 32]) -> Word {
    for byte in seed.iter_mut() {
        *byte = byte.wrapping_add(1);
        if *byte != 0 {
            break;
        }
    }
    Rpo256::hash(seed)
}

/// Generates a deterministic `AccountId` from a seed.
fn generate_account_id(seed: &mut [u8; 32]) -> AccountId {
    for byte in seed.iter_mut() {
        *byte = byte.wrapping_add(1);
        if *byte != 0 {
            break;
        }
    }
    AccountIdBuilder::new().build_with_seed(*seed)
}

// SETUP FUNCTIONS
// ================================================================================================

/// Sets up a vanilla `AccountTree` with specified number of accounts using `RocksDB` backend.
fn setup_vanilla_account_tree(
    num_accounts: usize,
) -> (AccountTree<LargeSmt<MemoryStorage>>, Vec<AccountId>) {
    let mut seed = [0u8; 32];
    let mut account_ids = Vec::new();
    let mut entries = Vec::new();

    for _ in 0..num_accounts {
        let account_id = generate_account_id(&mut seed);
        let commitment = generate_word(&mut seed);
        account_ids.push(account_id);
        entries.push((account_id_to_smt_key(account_id), commitment));
    }

    let storage = setup_storage();
    let smt =
        LargeSmt::with_entries(storage, entries).expect("Failed to create LargeSmt from entries");
    let tree = AccountTree::new(smt).expect("Failed to create AccountTree");
    (tree, account_ids)
}

/// Sets up `AccountTreeWithHistory` with specified number of accounts and blocks using `RocksDB`
/// backend.
fn setup_account_tree_with_history(
    num_accounts: usize,
    num_blocks: usize,
) -> (AccountTreeWithHistory<InMemoryAccountTree>, Vec<AccountId>) {
    let mut seed = [0u8; 32];
    let storage = setup_storage();
    let smt = LargeSmt::with_entries(storage, std::iter::empty())
        .expect("Failed to create empty LargeSmt");
    let account_tree = AccountTree::new(smt).expect("Failed to create AccountTree");
    let mut account_tree_hist = AccountTreeWithHistory::new(account_tree, BlockNumber::GENESIS);
    let mut account_ids = Vec::new();

    for _block in 0..num_blocks {
        let mutations: Vec<_> = (0..num_accounts)
            .map(|_| {
                let account_id = generate_account_id(&mut seed);
                let commitment = generate_word(&mut seed);
                if account_ids.len() < num_accounts {
                    account_ids.push(account_id);
                }
                (account_id, commitment)
            })
            .collect();

        account_tree_hist.compute_and_apply_mutations(mutations).unwrap();
    }

    (account_tree_hist, account_ids)
}

// VANILLA ACCOUNTTREE BENCHMARKS
// ================================================================================================

/// Benchmarks vanilla `AccountTree` open (query) operations.
/// This provides a baseline for comparison with historical access operations.
fn bench_vanilla_access(c: &mut Criterion) {
    let mut group = c.benchmark_group("account_tree_vanilla_access");

    let account_counts = [1, 10, 50, 100, 500, 1000];

    for &num_accounts in &account_counts {
        let (tree, account_ids) = setup_vanilla_account_tree(num_accounts);

        group.bench_function(BenchmarkId::new("vanilla", num_accounts), |b| {
            let test_account = *account_ids.first().unwrap();
            b.iter(|| {
                tree.open(black_box(test_account));
            });
        });
    }

    group.finish();
}

/// Benchmarks vanilla `AccountTree` insertion (mutation) performance.
/// This provides a baseline for comparison with history-tracking insertion.
fn bench_vanilla_insertion(c: &mut Criterion) {
    let mut group = c.benchmark_group("account_tree_insertion");

    let account_counts = [1, 10, 50, 100, 500];

    for &num_accounts in &account_counts {
        group.bench_function(BenchmarkId::new("vanilla", num_accounts), |b| {
            b.iter(|| {
                let mut seed = [0u8; 32];
                let storage = setup_storage();
                let smt = LargeSmt::with_entries(storage, std::iter::empty())
                    .expect("Failed to create empty LargeSmt");
                let mut tree = AccountTree::new(smt).expect("Failed to create AccountTree");
                let entries: Vec<_> = (0..num_accounts)
                    .map(|_| {
                        let account_id = generate_account_id(&mut seed);
                        let commitment = generate_word(&mut seed);
                        (account_id, commitment)
                    })
                    .collect();
                let mutations = tree.compute_mutations(black_box(entries)).unwrap();
                tree.apply_mutations(black_box(mutations)).unwrap();
            });
        });
    }

    group.finish();
}

// HISTORICAL ACCOUNTTREE BENCHMARKS
// ================================================================================================

/// Benchmarks historical access at different depths and account counts.
fn bench_historical_access(c: &mut Criterion) {
    let mut group = c.benchmark_group("account_tree_historical_access");

    let account_counts = [10, 100, 500, 2500];
    let block_depths = [0, 5, 10, 20, 32];

    for &num_accounts in &account_counts {
        for &block_depth in &block_depths {
            if block_depth > AccountTreeWithHistory::<InMemoryAccountTree>::MAX_HISTORY {
                continue;
            }

            let (tree_hist, account_ids) =
                setup_account_tree_with_history(num_accounts, block_depth + 1);
            let current_block = tree_hist.block_number_latest();
            let target_block = current_block
                .checked_sub(u32::try_from(block_depth).unwrap())
                .unwrap_or(BlockNumber::GENESIS);

            if block_depth >= tree_hist.history_len() && block_depth > 0 {
                continue;
            }

            group.bench_function(
                BenchmarkId::new(format!("depth_{block_depth}"), num_accounts),
                |b| {
                    let test_account = *account_ids.first().unwrap();
                    b.iter(|| {
                        tree_hist.open_at(black_box(test_account), black_box(target_block));
                    });
                },
            );
        }
    }

    group.finish();
}

/// Benchmarks insertion performance with history tracking at different account counts.
fn bench_insertion_with_history(c: &mut Criterion) {
    let mut group = c.benchmark_group("account_tree_insertion");

    let account_counts = [1, 10, 50, 100, 500, 2500];

    for &num_accounts in &account_counts {
        group.bench_function(BenchmarkId::new("with_history", num_accounts), |b| {
            b.iter(|| {
                let mut seed = [0u8; 32];
                let storage = setup_storage();
                let smt = LargeSmt::with_entries(storage, std::iter::empty())
                    .expect("Failed to create empty LargeSmt");
                let account_tree = AccountTree::new(smt).expect("Failed to create AccountTree");
                let mut tree = AccountTreeWithHistory::new(account_tree, BlockNumber::GENESIS);
                let mutations: Vec<_> = (0..num_accounts)
                    .map(|_| {
                        let account_id = generate_account_id(&mut seed);
                        let commitment = generate_word(&mut seed);
                        (account_id, commitment)
                    })
                    .collect();
                tree.compute_and_apply_mutations(black_box(mutations)).unwrap();
            });
        });
    }

    group.finish();
}

criterion_group!(
    name = historical_account_tree;
    config = Criterion::default()
        .measurement_time(std::time::Duration::from_millis(1500))
        .warm_up_time(std::time::Duration::from_millis(100))
        .sample_size(10);
    targets = bench_vanilla_access,
        bench_vanilla_insertion,
        bench_historical_access,
        bench_insertion_with_history
);
criterion_main!(historical_account_tree);
