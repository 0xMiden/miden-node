use std::hint::black_box;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use miden_crypto::hash::rpo::Rpo256;
use miden_node_store::historical::AccountTreeWithHistory;
use miden_objects::account::AccountId;
use miden_objects::block::{AccountTree, BlockNumber};
use miden_objects::testing::account_id::AccountIdBuilder;
use miden_objects::Word;

/// Generate a deterministic word from a seed
fn generate_word(seed: &mut [u8; 32]) -> Word {
    // Increment seed
    for i in 0..32 {
        seed[i] = seed[i].wrapping_add(1);
        if seed[i] != 0 {
            break;
        }
    }

    let digest = Rpo256::hash(seed);
    digest.into()
}

/// Generate a deterministic AccountId from a seed
fn generate_account_id(seed: &mut [u8; 32]) -> AccountId {
    // Increment seed
    for i in 0..32 {
        seed[i] = seed[i].wrapping_add(1);
        if seed[i] != 0 {
            break;
        }
    }

    AccountIdBuilder::new().build_with_seed(*seed)
}

/// Setup AccountTreeWithHistory with specified number of accounts and blocks
fn setup_account_tree_with_history(
    num_accounts: usize,
    num_blocks: usize,
) -> (AccountTreeWithHistory, Vec<AccountId>) {
    let mut seed = [0u8; 32];
    let account_tree_hist = AccountTreeWithHistory::new(AccountTree::new(), BlockNumber::GENESIS);
    let mut account_ids = Vec::new();

    // Apply mutations across multiple blocks to create history
    for _block in 0..num_blocks {
        let mut mutations = Vec::new();
        for _acct_idx in 0..num_accounts {
            let account_id = generate_account_id(&mut seed);
            let commitment = generate_word(&mut seed);
            mutations.push((account_id, commitment));

            if account_ids.len() < num_accounts {
                account_ids.push(account_id);
            }
        }

        account_tree_hist.apply_mutations(mutations).unwrap();
    }

    (account_tree_hist, account_ids)
}

/// Create vanilla AccountTree at different block states
fn create_vanilla_states(
    num_accounts: usize,
    num_blocks: usize,
) -> (Vec<AccountTree>, Vec<AccountId>) {
    let mut seed = [0u8; 32];
    let mut states = Vec::new();
    let mut current_tree = AccountTree::new();
    let mut account_ids = Vec::new();

    states.push(current_tree.clone());

    for _block in 0..num_blocks {
        for _acct_idx in 0..num_accounts {
            let account_id = generate_account_id(&mut seed);
            let commitment = generate_word(&mut seed);
            let _ = current_tree.insert(account_id, commitment);

            if account_ids.len() < num_accounts {
                account_ids.push(account_id);
            }
        }
        states.push(current_tree.clone());
    }

    (states, account_ids)
}

/// Benchmark historical access at different depths
fn bench_historical_access(c: &mut Criterion) {
    let mut group = c.benchmark_group("account_tree_historical_access");

    // Test configurations
    let account_counts = vec![1, 10, 100];
    let block_depths = vec![0, 1, 5, 10, 20];

    for num_accounts in account_counts {
        for &block_depth in &block_depths {
            // Skip impossible configurations
            if block_depth > AccountTreeWithHistory::MAX_HISTORY {
                continue;
            }

            let bench_id =
                BenchmarkId::new(format!("historical/{}_accounts", num_accounts), block_depth);

            let (tree_hist, account_ids) =
                setup_account_tree_with_history(num_accounts, block_depth + 1);
            let current_block = tree_hist.block_number();
            let target_block = current_block
                .checked_sub(block_depth as u32)
                .unwrap_or(BlockNumber::GENESIS);

            if block_depth >= tree_hist.history_len() && block_depth > 0 {
                continue;
            }

            // Benchmark AccountTreeWithHistory
            group.bench_function(bench_id, move |b| {
                let test_account = account_ids.get(0).copied().unwrap();

                b.iter(|| {
                    tree_hist.open_at(black_box(test_account), black_box(target_block));
                });
            });

            // Benchmark vanilla approach
            let bench_id_vanilla = BenchmarkId::new(
                format!("vanilla/{}_accounts", num_accounts),
                block_depth,
            );

            group.bench_function(bench_id_vanilla, |b| {
                let (states, account_ids) = create_vanilla_states(num_accounts, block_depth + 1);
                let test_account = account_ids.get(0).copied().unwrap();

                b.iter(|| {
                    // Access the state at the requested block depth
                    let state_idx = states.len() - 1 - block_depth;
                    states[state_idx].open(black_box(test_account))
                });
            });
        }
    }

    group.finish();
}

/// Benchmark insertion performance with history tracking
fn bench_insertion_with_history(c: &mut Criterion) {
    let mut group = c.benchmark_group("account_tree_insertion");

    let account_counts = vec![10, 100];

    for num_accounts in account_counts {
        // Benchmark AccountTreeWithHistory insertions
        group.bench_function(BenchmarkId::new("with_history", num_accounts), |b| {
            b.iter_batched(
                || {
                    let tree =
                        AccountTreeWithHistory::new(AccountTree::new(), BlockNumber::GENESIS);
                    let mut seed = [0u8; 32];
                    let mutations = Vec::from_iter((0..num_accounts).map(|_| {
                        let account_id = generate_account_id(&mut seed);
                        let commitment = generate_word(&mut seed);
                        (account_id, commitment)
                    }));
                    (tree, mutations)
                },
                |(tree, mutations)| {
                    tree.apply_mutations(black_box(mutations)).unwrap();
                },
                BatchSize::SmallInput,
            );
        });

        // Benchmark vanilla AccountTree insertions
        group.bench_function(BenchmarkId::new("vanilla", num_accounts), |b| {
            b.iter_batched(
                || {
                    let tree = AccountTree::new();
                    let mut seed = [0u8; 32];
                    let mutations = Vec::from_iter((0..num_accounts).map(|_| {
                        let account_id = generate_account_id(&mut seed);
                        let commitment = generate_word(&mut seed);
                        (account_id, commitment)
                    }));
                    (tree, mutations)
                },
                |(mut tree, mutations)| {
                    for (account_id, commitment) in mutations {
                        let _ = tree.insert(black_box(account_id), black_box(commitment));
                    }
                },
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

/// Benchmark proof generation at different historical depths
fn bench_proof_generation(c: &mut Criterion) {
    let mut group = c.benchmark_group("account_tree_proof_generation");

    let account_counts = vec![10, 100];
    let block_depths = vec![0, 5, 10, 20];

    for num_accounts in account_counts {
        for &block_depth in &block_depths {
            if block_depth >= AccountTreeWithHistory::MAX_HISTORY {
                continue;
            }

            let bench_id = BenchmarkId::new(
                format!("historical_proof/{}_accounts", num_accounts),
                block_depth,
            );

            let (tree_hist, account_ids) =
                setup_account_tree_with_history(num_accounts, block_depth + 1);
            let current_block = tree_hist.block_number();
            let target_block = current_block
                .checked_sub(block_depth as u32)
                .unwrap_or(BlockNumber::GENESIS);

            if block_depth >= tree_hist.history_len() && block_depth > 0 {
                continue;
            }

            group.bench_function(bench_id, move |b| {
                let test_account = account_ids.get(0).copied().unwrap();

                b.iter(|| tree_hist.open_at(black_box(test_account), black_box(target_block)));
            });
        }
    }

    group.finish();
}

/// Benchmark root retrieval at different historical depths
fn bench_root_retrieval(c: &mut Criterion) {
    let mut group = c.benchmark_group("account_tree_root_retrieval");

    let account_counts = vec![10, 100];
    let block_depths = vec![0, 5, 10, 20];

    for num_accounts in account_counts {
        for &block_depth in &block_depths {
            if block_depth >= AccountTreeWithHistory::MAX_HISTORY {
                continue;
            }

            let bench_id = BenchmarkId::new(
                format!("historical_root/{}_accounts", num_accounts),
                block_depth,
            );

            let (tree_hist, _) = setup_account_tree_with_history(num_accounts, block_depth + 1);
            let current_block = tree_hist.block_number();
            let target_block = current_block
                .checked_sub(block_depth as u32)
                .unwrap_or(BlockNumber::GENESIS);

            if block_depth >= tree_hist.history_len() && block_depth > 0 {
                continue;
            }

            group.bench_function(bench_id, move |b| {
                b.iter(|| {
                    tree_hist.root_at(black_box(target_block));
                });
            });
        }
    }

    group.finish();
}

criterion_group!(
    name = historical_account_tree;
    config = Criterion::default()
        .measurement_time(std::time::Duration::from_secs(2))
        .warm_up_time(std::time::Duration::from_secs(1))
        .sample_size(10);
    targets = bench_historical_access,
        bench_insertion_with_history,
        bench_proof_generation,
        bench_root_retrieval
);
criterion_main!(historical_account_tree);
