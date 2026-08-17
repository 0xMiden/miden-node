# Miden node stress test

`stress-test` is a development binary for generating local store data and running stress tests against Miden node store
workflows. It is part of the Miden node repository but is not published as a crates.io package.

## Role

The binary can seed a local store with generated accounts and then run focused benchmarks against store operations such
as state loading, account lookup, note sync, nullifier sync, transaction sync, and chain MMR sync.

This tool is intended for development and performance investigation. Benchmark numbers are sensitive to hardware,
database contents, feature flags, and the exact commit under test, so the reference results below should be treated as a
point-in-time comparison rather than current guarantees.

## Operation

Use the binary help output for the current command and configuration surface. The help output is the source of truth for
flags and environment variables.

### Large public account storage map

`seed-store` can create an exact number of accounts with a deterministic storage map on every public account. The
following command creates one public account containing 250,000 entries in one map, then applies one partial account
update that changes a single entry in that map:

```sh
cargo run --release --locked -p miden-node-stress-test -- \
  seed-store \
  --data-directory /tmp/miden-large-storage-map \
  --num-accounts 1 \
  --public-accounts-percentage 100 \
  --storage-map-entries 250000 \
  --vault-entries 1 \
  --account-update-blocks 1
```

Account creation and account updates each require a preceding note-emission block. The block metrics label these phases
separately, so `account-update` rows measure the partial public-account updates rather than their setup work. The
generated account IDs are written to `accounts.txt` in the data directory. If a full public account state would exceed
the protocol's transaction account-update size limit, `seed-store` inserts that account at genesis automatically; its
subsequent partial updates still use normal blocks and appear as `account-update` rows.

## Benchmark Results

The following reference results were obtained using a store with 100k accounts, half of which are public.

### Seed Metrics

```text
Total time: 235.452 seconds
Inserted 393 blocks with avg insertion time 212 ms
Initial DB size: 120.1 KB
Average DB growth rate: 325.3 KB per block
```

### Block Metrics

Each block contains 256 transactions (16 batches \* 16 transactions).

| Block | Insert Time (ms) | Get Block Inputs Time (ms) | Get Batch Inputs Time (ms) | Block Size (KB) | DB Size (MB) |
| ----- | ---------------- | -------------------------- | -------------------------- | --------------- | ------------ |
| 0     | 22               | 1                          | 0                          | 375.6           | 0.3          |
| 50    | 186              | 9                          | 1                          | 473.6           | 22.2         |
| 100   | 199              | 10                         | 1                          | 473.6           | 40.7         |
| 150   | 219              | 10                         | 1                          | 473.6           | 58.1         |
| 200   | 218              | 11                         | 1                          | 473.6           | 74.8         |
| 250   | 222              | 11                         | 1                          | 473.6           | 91.6         |
| 300   | 228              | 12                         | 1                          | 473.6           | 108.1        |
| 350   | 232              | 13                         | 1                          | 473.6           | 124.4        |

### Database Stats

The database contained 100215 accounts and 100215 notes across all blocks.

| Table                              | Size (MB) | KB/Entry |
| ---------------------------------- | --------- | -------- |
| accounts                           | 26.1      | 0.3      |
| account_deltas                     | 1.2       | 0.0      |
| account_fungible_asset_deltas      | 2.2       | 0.0      |
| account_non_fungible_asset_updates | 0.0       | -        |
| account_storage_map_updates        | 0.0       | -        |
| account_storage_slot_updates       | 3.1       | 0.1      |
| block_headers                      | 0.1       | 0.3      |
| notes                              | 49.1      | 0.5      |
| note_scripts                       | 0.0       | 8.0      |
| nullifiers                         | 4.6       | 0.0      |
| transactions                       | 6.0       | 0.1      |

### Index Stats

| Index                        | Size (MB) |
| ---------------------------- | --------- |
| idx_accounts_network_prefix  | 0.0       |
| idx_notes_note_id            | 4.4       |
| idx_notes_sender             | 2.9       |
| idx_notes_tag                | 1.6       |
| idx_notes_nullifier          | 4.4       |
| idx_unconsumed_network_notes | 1.1       |
| idx_nullifiers_prefix        | 4.3       |
| idx_nullifiers_block_num     | 4.2       |
| idx_transactions_account_id  | 5.6       |
| idx_transactions_block_num   | 4.2       |

### Store Stress Tests

Latency measurements represent pure store processing time without network overhead.

#### load-state

Measures full store startup (`State::load`) against the seeded data directory. `--load-iterations` (default 3) repeats
the load; the first iteration may pay RocksDB WAL recovery and a cold OS page cache, while later iterations measure a
clean warm restart.

```text
Iteration 0: state loaded in 38.623292ms
Iteration 1: state loaded in 20.376417ms
Iteration 2: state loaded in 17.526916ms
...
Database contains 52 accounts and 50 nullifiers
```

Build with `--features tracing-forest` to render the per-phase breakdown of each load as a timing tree, including the
RocksDB opens (`open_tree_storage`, `open_forest_storage`):

```text
INFO     load [ 36.1ms | 0.00% / 100.00% ]
INFO     ┕━ load_with_database_options [ 36.1ms | 0.00% / 100.00% ]
INFO        ┝━ load_with_pool_size [ 8.47ms | 23.48% ]
INFO        ┝━ load_mmr [ 1.27ms | 3.52% ]
INFO        ┝━ open_tree_storage [ 10.3ms | 28.68% ] path: "accounttree"
INFO        ┝━ load_account_tree [ 2.19ms | 6.08% ]
INFO        ┝━ open_tree_storage [ 6.38ms | 17.70% ] path: "nullifiertree"
INFO        ┝━ load_nullifier_tree [ 822µs | 2.28% ]
INFO        ┝━ verify_tree_consistency [ 68.0µs | 0.19% ]
INFO        ┝━ open_forest_storage [ 5.98ms | 16.60% ] path: "accountstateforest"
INFO        ┝━ load_account_state_forest [ 74.4µs | 0.21% ] block.number: 2
INFO        ┕━ verify_account_state_forest_consistency [ 458µs | 1.27% ]
```

#### sync-notes

```text
Average request latency: 653.751us
P50 request latency: 606.417us
P95 request latency: 1.044666ms
P99 request latency: 1.528667ms
P99.9 request latency: 5.247875ms
```

#### sync-nullifiers

```text
Average request latency: 519.239us
P50 request latency: 503.708us
P95 request latency: 747.333us
P99 request latency: 873.083us
P99.9 request latency: 2.289709ms
Average nullifiers per response: 21.0348
```

#### sync-transactions

```text
Average request latency: 1.61454ms
P50 request latency: 1.439584ms
P95 request latency: 3.195001ms
P99 request latency: 4.068709ms
P99.9 request latency: 6.888542ms
Average transactions per response: 1.547
Pagination statistics:
  Total runs: 10000
  Runs triggering pagination: 9971
  Pagination rate: 99.71%
  Average pages per run: 2.00
```

#### sync-chain-mmr

```text
Average request latency: 1.021ms
P50 request latency: 0.981ms
P95 request latency: 1.412ms
P99 request latency: 1.822ms
P99.9 request latency: 3.174ms
Pagination statistics:
  Total runs: 10000
  Runs triggering pagination: 1
  Pagination rate: 0.01%
  Average pages per run: 1.00
```

#### get-account

```text
Average request latency: 937.969us
P50 request latency: 688.332us
P95 request latency: 932.549us
P99 request latency: 1.119977ms
P99.9 request latency: 42.992839ms
GetAccount statistics:
  Total runs: 10000
  Storage map limit exceeded responses: 0
  Average returned storage map entries: 64.00
  Vault limit exceeded responses: 0
  Average returned vault assets: 2.00
```

## License

This project is [MIT licensed](../../LICENSE).
