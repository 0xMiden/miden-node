use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use miden_crypto::hash::rpo::Rpo256;
use miden_node_store::historical::AccountTreeWithHistory;
use miden_objects::Word;
use miden_objects::account::AccountId;
use miden_objects::block::{AccountTree, BlockNumber};
use miden_objects::testing::account_id::AccountIdBuilder;

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

/// Sets up `AccountTreeWithHistory` with specified number of accounts and blocks.
fn setup_account_tree_with_history(
    num_accounts: usize,
    num_blocks: usize,
) -> (AccountTreeWithHistory, Vec<AccountId>) {
    let mut seed = [0u8; 32];
    let account_tree_hist = AccountTreeWithHistory::new(AccountTree::new(), BlockNumber::GENESIS);
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

        account_tree_hist.apply_mutations(mutations).unwrap();
    }

    (account_tree_hist, account_ids)
}

/// Benchmarks historical access at different depths.
fn bench_historical_access(c: &mut Criterion) {
    let mut group = c.benchmark_group("account_tree_historical_access");

    let num_accounts = 10;
    let block_depths = [0, 5, 10];

    for &block_depth in &block_depths {
        if block_depth > AccountTreeWithHistory::MAX_HISTORY {
            continue;
        }

        let (tree_hist, account_ids) =
            setup_account_tree_with_history(num_accounts, block_depth + 1);
        let current_block = tree_hist.block_number();
        let target_block = current_block
            .checked_sub(u32::try_from(block_depth).unwrap())
            .unwrap_or(BlockNumber::GENESIS);

        if block_depth >= tree_hist.history_len() && block_depth > 0 {
            continue;
        }

        group.bench_function(
            BenchmarkId::new(format!("historical_{num_accounts}_accounts"), block_depth),
            |b| {
                let test_account = *account_ids.first().unwrap();
                b.iter(|| {
                    tree_hist.open_at(black_box(test_account), black_box(target_block));
                });
            },
        );
    }

    group.finish();
}

/// Benchmarks insertion performance with history tracking.
fn bench_insertion_with_history(c: &mut Criterion) {
    let mut group = c.benchmark_group("account_tree_insertion");

    let num_accounts = 10;

    group.bench_function(BenchmarkId::new("with_history", num_accounts), |b| {
        b.iter(|| {
            let mut seed = [0u8; 32];
            let tree = AccountTreeWithHistory::new(AccountTree::new(), BlockNumber::GENESIS);
            let mutations: Vec<_> = (0..num_accounts)
                .map(|_| {
                    let account_id = generate_account_id(&mut seed);
                    let commitment = generate_word(&mut seed);
                    (account_id, commitment)
                })
                .collect();
            tree.apply_mutations(black_box(mutations)).unwrap();
        });
    });

    group.finish();
}

criterion_group!(
    name = historical_account_tree;
    config = Criterion::default()
        .measurement_time(std::time::Duration::from_secs(2))
        .warm_up_time(std::time::Duration::from_secs(1))
        .sample_size(10);
    targets = bench_historical_access,
        bench_insertion_with_history
);
criterion_main!(historical_account_tree);
